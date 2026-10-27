// SPDX-License-Identifier: MIT
//! Loop and orchestrator notifications: console, toast, file, email (SMTP or HTTP provider) and webhook.
//!
//! Configuration (the app `.env`, a real environment variable wins): LOOP_NOTIFY_CHANNELS (console,toast,file,email,webhook; a channel
//! without configuration is skipped, never breaks the loop), LOOP_NOTIFY_TO, LOOP_EMAIL_PROVIDER, LOOP_SMTP_*, LOOP_EMAIL_HTTP_*,
//! LOOP_WEBHOOK_URL. The standard message of every channel is the REASON the loop stopped plus one line that sends you to the panel.
//! `harness notify --test [--channels email]` | `--subject S --body T` | `--subject S --body-file F`.

use crate::email;
use crate::loop_state::loop_path;
use cfg::env::load_all;
use cfg::Env;
use pycompat::json::py_json_dumps;
use pycompat::Re;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

const MAX_BODY: usize = 6000;
const MAX_SHORT: usize = 950;
const NOTIFY_USAGE: &str = "usage: notify.py [-h] [--test] [--subject SUBJECT] [--body BODY]
                 [--body-file BODY_FILE] [--channels CHANNELS]

Loop notices (console, toast, file, email, webhook)

options:
  -h, --help            show this help message and exit
  --test                sends a test notice through the configured channels
  --subject SUBJECT
  --body BODY
  --body-file BODY_FILE
  --channels CHANNELS   overrides LOOP_NOTIFY_CHANNELS (e.g. email,console)
";
const CHAT_NOTE: &str =
    "Se você está numa CONVERSA NORMAL (chat do Claude com /orchestrator): NÃO rode o script. Responda direto no chat; o orquestrador segue normalmente.";
const CRITICAL_DECISION: &str = "critical decision: ";

static DIRTY_TREE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"(?s)dirty tree outside \.specs/ (?:in repo (\S+) )?before turn (\d+)[^:]*:\s*(.*?)(?:\.\s*In the panel use.*)?\z"));
static TURN_TIMEOUT: LazyLock<Re> = LazyLock::new(|| Re::new(r"turn (\d+) exceeded (\d+) min"));
static COST_CAP: LazyLock<Re> = LazyLock::new(|| Re::new(r"total cost cap exceeded: US\$ ([\d.]+) > ([\d.]+)"));
static COST_CAP_PAUSE: LazyLock<Re> = LazyLock::new(|| Re::new(r"cost cap reached: US\$ ([\d.]+) of ([\d.]+)"));
static TURN_FAILED: LazyLock<Re> = LazyLock::new(|| Re::new(r"claude exited with code (-?\d+) on turn (\d+)"));
static STALL: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(\d+) consecutive turns without ROADMAP progress"));
static PORCELAIN: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s*[A-Z?!]{1,2}\s+(.*)\z"));

fn title_of(event: &str) -> Option<&'static str> {
    Some(match event {
        "awaiting_user" => "Aguardando sua decisão",
        "error" => "Execução interrompida com erro",
        "paused" => "Execução pausada",
        "done" => "Execução concluída",
        "feature_verified" => "Feature verificada",
        "stage_closed" => "Etapa concluída",
        "test" => "Teste de notificação",
        "custom" => "Aviso",
        _ => return None,
    })
}

/// Cuts `text` to `size` characters at a sentence end when one falls in the second half, else at a word boundary (never in the middle of a
/// word), and marks the cut with `...`.
fn clip(text: &str, size: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= size {
        return text.to_string();
    }
    let room = size.saturating_sub(3);
    let head: String = text.chars().take(room).collect();
    let next_is_break = text.chars().nth(room).is_none_or(char::is_whitespace);
    let sentence = head.rfind(['.', '!', '?', '\n']).filter(|i| head[..*i].chars().count() >= room / 2);
    let cut = match sentence {
        Some(i) => head[..=i].to_string(),
        None if next_is_break => head,
        None => match head.rfind(char::is_whitespace) {
            Some(i) if head[..i].chars().count() >= room / 2 => head[..i].to_string(),
            _ => head,
        },
    };
    format!("{}...", cut.trim_end())
}

fn s<'a>(ctx: &'a Value, key: &str) -> &'a str {
    ctx.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Python truthiness of an optional JSON value.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(x)) => !x.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn event(ctx: &Value) -> &str {
    ctx.get("event").and_then(Value::as_str).unwrap_or("custom")
}

fn string_list(ctx: &Value, key: &str) -> Vec<String> {
    ctx.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|v| v.as_str().map(String::from).unwrap_or_else(|| py_json_dumps(v, false))).collect())
        .unwrap_or_default()
}

fn command_base(ctx: &Value) -> String {
    format!(
        "python \"{}\" \"{}\"{}",
        ctx.get("script").and_then(Value::as_str).unwrap_or("<run_loop.py>"),
        ctx.get("repo").and_then(Value::as_str).unwrap_or("<repo>"),
        s(ctx, "flags")
    )
}

/// The default: a notice says WHY the loop stopped and sends the reader to the panel. LOOP_NOTIFY_CONTINUE=commands brings back the long form.
fn continue_in_panel(env: &Env) -> bool {
    let v = env.text("LOOP_NOTIFY_CONTINUE");
    let v = if v.trim().is_empty() { "panel".to_string() } else { v.trim().to_lowercase() };
    v != "commands"
}

fn needs_continue_block(ctx: &Value) -> bool {
    ["awaiting_user", "error", "paused"].contains(&event(ctx))
}

fn app_harness() -> String {
    std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "harness".into())
}

fn command_set(ctx: &Value) -> Vec<String> {
    let base = command_base(ctx);
    match event(ctx) {
        "awaiting_user" => {
            let mut lines = Vec::new();
            if truthy(ctx.get("options")) {
                lines.push(format!("{base} --option 1"));
            }
            lines.push(format!("{base} --answer \"WRITE YOUR MESSAGE HERE\""));
            lines
        }
        "test" => vec![format!("{base} --answer \"WRITE YOUR MESSAGE HERE\"")],
        "error" | "paused" => vec![base],
        _ => vec![],
    }
}

fn subject_of(ctx: &Value) -> String {
    let ev = event(ctx);
    if ev == "custom" {
        return if s(ctx, "subject").is_empty() { "[loop] aviso".into() } else { s(ctx, "subject").to_string() };
    }
    let title = title_of(ev).unwrap_or(ev);
    let mut named = string_list(ctx, "verified_now");
    if named.is_empty() && !["feature_verified", "stage_closed"].contains(&ev) {
        named = string_list(ctx, "running"); // a stop names the feature that was running
    }
    let stage = s(ctx, "stage").trim();
    let mut parts: Vec<String> = Vec::new();
    if !named.is_empty() {
        parts.push(named.iter().take(3).cloned().collect::<Vec<_>>().join(", ") + if named.len() > 3 { " e outras" } else { "" });
    }
    if !stage.is_empty() {
        parts.push(format!("etapa {stage}"));
    }
    if parts.is_empty() || ev == "test" {
        format!("[Claude-Maestro] {title}")
    } else {
        format!("[Claude-Maestro] {title}: {}", parts.join(" | "))
    }
}

fn loop_id(ctx: &Value) -> String {
    let branch = s(ctx, "branch");
    let tail = branch.split_once('/').map(|(_, t)| t).unwrap_or(branch);
    if tail.is_empty() {
        "-".into()
    } else {
        tail.to_string()
    }
}

/// The one vocabulary of every channel (N8): "verificada" is a feature that passed its checks and sits in the loop branch; it is
/// "entregue" only after the branch was pushed (and the PR opened). The word "concluída/finalizada/done" is not used for a feature.
const FEATURE_GROUPS: [(&str, &[&str]); 6] = [
    ("verificadas", &["verified"]),
    ("com especificação aprovada", &["specced", "approved"]),
    ("em execução", &["building"]),
    ("em especificação", &["speccing"]),
    ("pendentes", &["pending"]),
    ("bloqueadas", &["blocked"]),
];

/// `(closed, total, [(label, count)])`.
fn feature_counts(ctx: &Value) -> (usize, usize, Vec<(&'static str, usize)>) {
    let statuses: Vec<String> =
        ctx.get("features").and_then(Value::as_array).map(|a| a.iter().filter_map(|p| p.get(1).and_then(Value::as_str).map(String::from)).collect()).unwrap_or_default();
    let groups: Vec<(&str, usize)> =
        FEATURE_GROUPS.iter().map(|(l, names)| (*l, statuses.iter().filter(|s| names.contains(&s.as_str())).count())).filter(|(_, n)| *n > 0).collect();
    (statuses.iter().filter(|s| *s == "verified").count(), statuses.len(), groups)
}

/// Status word of one feature in the notice.
fn status_label(status: &str) -> &'static str {
    match status {
        "verified" => "verificada",
        "building" => "em execução",
        "speccing" => "em especificação",
        "specced" | "approved" => "especificação aprovada",
        "pending" => "pendente",
        "blocked" => "bloqueada",
        _ => "sem status",
    }
}

/// Whether the work was delivered (branch pushed): only then the count says "entregues".
fn delivered(ctx: &Value) -> bool {
    let push = s(ctx, "push");
    ctx.get("push").is_some() && truthy(ctx.get("push")) && push.starts_with("done")
}

/// Lines `slug: status`, the running feature marked, for the notice body.
fn feature_lines(ctx: &Value) -> Vec<String> {
    let running = string_list(ctx, "running");
    ctx.get("features")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| Some((p.get(0)?.as_str()?, p.get(1)?.as_str()?)))
                .map(|(slug, status)| {
                    let now = if running.iter().any(|r| r == slug) { " (rodando agora)" } else { "" };
                    format!("{slug}: {}{now}", status_label(status))
                })
                .take(40)
                .collect()
        })
        .unwrap_or_default()
}

/// A stop reason that has no translation yet: no command-line flags, no raw engine tone, and a pointer to the panel.
fn plain_fallback(text: &str) -> String {
    let kept: Vec<&str> = text.split_whitespace().filter(|w| !w.starts_with("--")).collect();
    format!("O loop parou e o motivo técnico ainda não tem tradução: {} Abra o painel para ver os detalhes e a ação sugerida.", clip(&kept.join(" "), 700))
}

fn explain_reason(reason: &str) -> (String, Vec<String>) {
    let text = reason.trim();
    if let Some(r) = TURN_FAILED.search(text) {
        return (
            format!(
                "O assistente (Claude) encerrou com erro no turno {} (código {}). Veja o detalhe no painel e retome o loop quando o problema for resolvido.",
                r.group(2).unwrap_or(""),
                r.group(1).unwrap_or("")
            ),
            vec![],
        );
    }
    if text.contains("no account can be used") {
        return ("Nenhuma conta do Claude pode ser usada agora (login expirado, acesso desativado pela organização ou token revogado). Esperar não resolve: entre de novo na conta, peça ao administrador para liberar o acesso, ou configure outra conta de reserva nas configurações do painel.".to_string(), vec![]);
    }
    if text.starts_with("account usage limit persisted") {
        return ("O limite de uso de todas as contas continuou esgotado depois de várias esperas. Aguarde a renovação do limite ou configure outra conta de reserva no painel, e retome o loop.".to_string(), vec![]);
    }
    if text.starts_with("interrupted by the user") {
        return ("O loop foi interrompido por você (Ctrl+C). Nada foi perdido; retome pelo painel quando quiser.".to_string(), vec![]);
    }
    if text.starts_with("LOOP.json missing, stale or invalid") {
        return ("O turno terminou sem registrar corretamente o estado do loop (arquivo LOOP.json ausente ou inválido). No painel, volte ao turno anterior ou retome para tentar de novo.".to_string(), vec![]);
    }
    if text.starts_with("identical state") {
        return ("O loop parou porque dois turnos seguidos terminaram sem nenhuma mudança (roadmap, estado e commits iguais). Veja no painel o que está travando e responda ou ajuste a feature.".to_string(), vec![]);
    }
    if let Some(n) = STALL.search(text) {
        return (
            format!(
                "O loop parou depois de {} turnos seguidos sem avanço no roadmap. Veja no painel qual feature está travada e decida o que fazer com ela.",
                n.group(1).unwrap_or("")
            ),
            vec![],
        );
    }
    if text.starts_with("permission denied on turn") {
        return (
            "O assistente tentou um comando perigoso que o guarda bloqueou, então o loop parou por segurança. Confira o comando no painel antes de retomar.".to_string(),
            vec![],
        );
    }
    if text.starts_with("worktree is off the loop branch") {
        return (
            "A pasta de trabalho do loop saiu da branch do loop e não foi possível voltar. Confira no painel o estado do repositório antes de retomar.".to_string(),
            vec![],
        );
    }
    if text.starts_with("claude output on turn") {
        return ("A resposta do Claude não foi reconhecida (o formato da ferramenta pode ter mudado). Rode o diagnóstico do painel para comparar a versão instalada com a testada.".to_string(), vec![]);
    }
    if text.starts_with("all features verified or blocked") {
        return ("Todas as features foram verificadas ou bloqueadas.".to_string(), vec![]);
    }
    if let Some(d) = DIRTY_TREE.search(text) {
        let files: Vec<String> = d
            .group(3)
            .unwrap_or("")
            .split(';')
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(|r| PORCELAIN.replace_all(r, |m| m.group(1).unwrap_or("").to_string()))
            .collect();
        let where_ = d.group(1).filter(|g| !g.is_empty()).map(|g| format!(" no repositório {g}")).unwrap_or_default();
        let mut bullets = vec!["Arquivos com alterações não commitadas:".to_string()];
        bullets.extend(files);
        return (
            format!("A execução parou no turno {} porque foram detectadas alterações não salvas (dirty tree) fora da pasta .specs/{where_}. Como esses arquivos estão fora dos caminhos das features em andamento, o sistema não fez o reset automático. No painel, escolha commitar ou descartar.", d.group(2).unwrap_or("")),
            bullets,
        );
    }
    if let Some(t) = TURN_TIMEOUT.search(text) {
        return (
            format!(
                "O turno {} passou do limite de {} minutos e foi encerrado. Aumente o tempo do turno ou divida a feature.",
                t.group(1).unwrap_or(""),
                t.group(2).unwrap_or("")
            ),
            vec![],
        );
    }
    if let Some(r) = COST_CAP_PAUSE.search(text) {
        return (
            format!(
                "O loop foi pausado: o custo total (US$ {}) atingiu o limite de US$ {}. Nada foi perdido; aumente o limite no painel para continuar.",
                r.group(1).unwrap_or(""),
                r.group(2).unwrap_or("")
            ),
            vec![],
        );
    }
    if let Some(c) = COST_CAP.search(text) {
        return (
            format!(
                "O custo total (US$ {}) passou do limite configurado (US$ {}). Aumente o limite se quiser continuar.",
                c.group(1).unwrap_or(""),
                c.group(2).unwrap_or("")
            ),
            vec![],
        );
    }
    (plain_fallback(text), vec![])
}

type Section = (String, Option<String>, Vec<String>);

struct Notice {
    session: Vec<(String, String)>,
    progress: Option<(String, Vec<String>)>,
    sections: Vec<Section>,
    how: Option<String>,
}

fn now_br() -> String {
    time::OffsetDateTime::now_local()
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
        .format(time::macros::format_description!("[day]/[month]/[year] [hour]:[minute]"))
        .unwrap_or_default()
}

fn options_lines(ctx: &Value) -> Vec<String> {
    string_list(ctx, "options").iter().take(6).enumerate().map(|(n, o)| format!("Opção {}: {o}", n + 1)).collect()
}

fn compose_notice(ctx: &Value) -> Notice {
    let ev = event(ctx);
    let (closed, total, groups) = feature_counts(ctx);
    let mut session = vec![("ID do loop".to_string(), loop_id(ctx))];
    if truthy(ctx.get("turn")) {
        session.push(("Turno".into(), ctx.get("turn").map(|v| v.as_str().map(String::from).unwrap_or_else(|| py_json_dumps(v, false))).unwrap_or_default()));
    }
    if let Some(cost) = ctx.get("cost").and_then(Value::as_f64) {
        session.push(("Custo atual".into(), format!("US$ {cost:.2}")));
    }
    if !s(ctx, "stage").trim().is_empty() {
        session.push(("Etapa".into(), s(ctx, "stage").trim().to_string()));
    }
    session.push(("Quando".into(), now_br()));
    if truthy(ctx.get("monitor_url")) && !needs_continue_block(ctx) {
        session.push(("Painel".into(), s(ctx, "monitor_url").to_string()));
    }
    let progress = if total > 0 && ev != "test" {
        let word = if delivered(ctx) { "entregues" } else { "verificadas, ainda não entregues" };
        let mut lines: Vec<String> = groups.iter().map(|(l, n)| format!("{n} {l}")).collect();
        lines.extend(feature_lines(ctx));
        Some((format!("Status das features ({closed} de {total} {word})"), lines))
    } else {
        None
    };
    let mut notice = Notice { session, progress, sections: vec![], how: None };
    let reason = s(ctx, "reason");
    match ev {
        "test" => notice.sections.push(("Teste".into(), Some("Este é um aviso de TESTE do loop. Nada precisa ser feito.".into()), vec![])),
        "error" | "paused" => {
            let (paragraph, bullets) = explain_reason(reason);
            notice.sections.push(("Motivo da parada".into(), Some(paragraph), bullets));
        }
        "awaiting_user" if reason.starts_with(CRITICAL_DECISION) => {
            notice.sections.push(("Decisão crítica (critical decision) precisa da sua resposta".into(), Some(clip(reason, 1500)), options_lines(ctx)));
        }
        "awaiting_user" => {
            let text = if reason.is_empty() { "(sem texto: veja o resumo)".to_string() } else { clip(reason, 1500) };
            notice.sections.push(("Decisão necessária".into(), Some(text), options_lines(ctx)));
        }
        "feature_verified" | "stage_closed" => {
            let names = string_list(ctx, "verified_now");
            let mut lines: Vec<String> = names.iter().map(|n| format!("{n}: verificada (ainda não entregue)")).collect();
            let stages = string_list(ctx, "stage_closed");
            if !stages.is_empty() {
                lines.push(format!("Etapa concluída: {}", stages.join(", ")));
            }
            notice.sections.push((
                "O que mudou neste turno".into(),
                Some("O loop segue sozinho; este aviso é só informativo. Nada foi enviado ao repositório remoto ainda.".into()),
                lines,
            ));
            if truthy(ctx.get("commits")) {
                notice.sections.push(("Commits recentes".into(), None, string_list(ctx, "commits").iter().take(5).map(|i| clip(i, 120)).collect()));
            }
        }
        "done" => {
            let mut lines = Vec::new();
            if truthy(ctx.get("push")) {
                lines.push(format!("Push da branch: {}", clip(s(ctx, "push"), 200)));
            }
            if truthy(ctx.get("pr")) {
                let status = if truthy(ctx.get("pr_status")) { format!(" ({})", s(ctx, "pr_status")) } else { String::new() };
                lines.push(format!("PR: {}{status}", clip(s(ctx, "pr"), 300)));
            }
            let result = if delivered(ctx) {
                "Todas as features foram verificadas ou bloqueadas e a branch foi enviada (entregue)."
            } else {
                "Todas as features foram verificadas ou bloqueadas, mas nada foi entregue ainda (sem push)."
            };
            notice.sections.push(("Resultado".into(), Some(result.into()), lines));
            if truthy(ctx.get("validate")) {
                notice.sections.push((
                    "Para você validar (decidido sem parar você)".into(),
                    None,
                    string_list(ctx, "validate").iter().take(8).map(|i| clip(i, 200)).collect(),
                ));
            }
            if truthy(ctx.get("commits")) {
                notice.sections.push(("Commits".into(), None, string_list(ctx, "commits").iter().take(8).map(|i| clip(i, 120)).collect()));
            }
        }
        _ => {}
    }
    if truthy(ctx.get("commits")) && ["awaiting_user", "error", "paused"].contains(&ev) {
        notice.sections.push(("Commits recentes".into(), None, string_list(ctx, "commits").iter().take(5).map(|i| clip(i, 120)).collect()));
    }
    if truthy(ctx.get("to_check")) && ["awaiting_user", "error", "paused"].contains(&ev) {
        notice.sections.push(("Conferir antes de responder".into(), None, string_list(ctx, "to_check").iter().take(8).map(|i| clip(i, 200)).collect()));
    }
    if needs_continue_block(ctx) {
        notice.how = Some(
            if truthy(ctx.get("monitor_url")) {
                "Para resolver, responder, retomar ou aprovar, acesse o painel de controle:"
            } else {
                "Para continuar, acesse o painel de controle abrindo-o com o comando:"
            }
            .to_string(),
        );
    }
    notice
}

fn panel_value(ctx: &Value) -> String {
    if truthy(ctx.get("monitor_url")) {
        s(ctx, "monitor_url").to_string()
    } else {
        format!("\"{}\" serve --project \"{}\"", app_harness(), ctx.get("repo").and_then(Value::as_str).unwrap_or("<repo>"))
    }
}

/// "full" format: plain text (file, console, webhook).
pub fn build(env: &Env, ctx: &Value) -> (String, String) {
    let ev = event(ctx);
    let subject = subject_of(ctx);
    if ev == "custom" {
        return (subject, clip(&format!("{}\n\nCOMO CONTINUAR\nOrigem: conversa normal. {CHAT_NOTE}", s(ctx, "extra_body")), MAX_BODY));
    }
    let n = compose_notice(ctx);
    // the channel adds the subject (file: header line; console: banner), so the body does not repeat it
    let mut out: Vec<String> = vec!["Detalhes da sessão".into()];
    out.extend(n.session.iter().map(|(k, v)| format!("- {k}: {v}")));
    if let Some((title, lines)) = &n.progress {
        out.push(String::new());
        out.push(title.clone());
        out.extend(lines.iter().map(|l| format!("- {l}")));
    }
    for (heading, paragraph, items) in &n.sections {
        out.push(String::new());
        out.push(heading.clone());
        if let Some(p) = paragraph {
            out.push(p.clone());
        }
        out.extend(items.iter().map(|i| if i.ends_with(':') { i.clone() } else { format!("- {i}") }));
    }
    if truthy(ctx.get("graph")) && ev != "test" {
        out.extend([String::new(), "Andamento".into(), clip(s(ctx, "graph"), 1800)]);
    }
    let commands = command_set(ctx);
    if continue_in_panel(env) {
        if let Some(how) = &n.how {
            out.extend([String::new(), "Como continuar".into(), how.clone(), panel_value(ctx)]);
        }
    } else if !commands.is_empty() {
        out.extend([
            String::new(),
            "Como continuar".into(),
            "1) Loop pelo script: rode um dos comandos marcados com >> no seu PC. O primeiro responde pelo NÚMERO da opção; o segundo aceita texto livre.".into(),
        ]);
        out.extend(commands.iter().map(|c| format!(">> {c}")));
        out.extend([String::new(), format!("2) {CHAT_NOTE}")]);
        if truthy(ctx.get("monitor_url")) {
            out.extend([String::new(), format!("3) Responder no painel: {}", s(ctx, "monitor_url"))]);
        }
    }
    if truthy(ctx.get("summary")) {
        out.extend([String::new(), format!("Resumo completo: {}", s(ctx, "summary"))]);
    }
    (subject, clip(&out.join("\n"), MAX_BODY))
}

/// `html.escape(text)` (quote=True).
fn esc(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#x27;")
}

/// "html" format: structured email.
pub fn build_html(env: &Env, ctx: &Value) -> (String, String) {
    let ev = event(ctx);
    let subject = subject_of(ctx);
    let heading = |t: &str| format!("<h3 style=\"margin:18px 0 6px;font-size:15px;color:#111827\">{}</h3>", esc(t));
    let para = |t: &str| format!("<p style=\"margin:0 0 8px;line-height:1.45\">{}</p>", esc(t));
    let bullets =
        |items: &[String]| format!("<ul style=\"margin:0 0 8px;padding-left:20px\">{}</ul>", items.iter().map(|i| format!("<li>{}</li>", esc(i))).collect::<String>());
    let code = |lines: &[String]| {
        format!("<pre style=\"background:#0f172a;color:#e2e8f0;padding:12px 14px;border-radius:6px;font-family:Consolas,Menlo,monospace;font-size:13px;line-height:1.5;white-space:pre-wrap;word-break:break-all;margin:6px 0 10px\">{}</pre>", esc(&lines.join("\n")))
    };
    let mut out: Vec<String> = vec![
        "<div style=\"font-family:Segoe UI,Arial,sans-serif;font-size:14px;color:#1f2937;max-width:680px\">".into(),
        format!("<h2 style=\"margin:0 0 8px;font-size:18px\">{}</h2>", esc(&subject)),
    ];
    if ev == "custom" {
        out.push(format!("<div style=\"white-space:pre-wrap;line-height:1.45\">{}</div>", esc(s(ctx, "extra_body"))));
        out.push(heading("Como continuar"));
        out.push(para(&format!("Origem: conversa normal. {CHAT_NOTE}")));
        out.push("</div>".into());
        return (subject, out.concat());
    }
    let n = compose_notice(ctx);
    out.push(heading("Detalhes da sessão"));
    out.push(bullets(&n.session.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>()));
    if let Some((title, lines)) = &n.progress {
        out.push(heading(title));
        out.push(bullets(lines));
    }
    for (title, paragraph, items) in &n.sections {
        out.push(heading(title));
        if let Some(p) = paragraph {
            out.push(para(p));
        }
        let labels: Vec<&String> = items.iter().filter(|i| i.ends_with(':')).collect();
        out.extend(labels.iter().map(|l| para(l)));
        if labels.len() != items.len() {
            out.push(bullets(&items.iter().filter(|i| !i.ends_with(':')).cloned().collect::<Vec<_>>()));
        }
    }
    if truthy(ctx.get("graph")) && ev != "test" {
        out.push(heading("Andamento"));
        out.push(format!("<pre style=\"background:#f3f4f6;padding:10px 12px;border-radius:6px;font-family:Consolas,Menlo,monospace;font-size:12.5px;line-height:1.45;white-space:pre-wrap\">{}</pre>", esc(&clip(s(ctx, "graph"), 1800))));
    }
    let commands = command_set(ctx);
    if continue_in_panel(env) {
        if let Some(how) = &n.how {
            let link = if truthy(ctx.get("monitor_url")) {
                format!("<p style=\"margin:0 0 8px\"><a href=\"{u}\">{u}</a></p>", u = esc(s(ctx, "monitor_url")))
            } else {
                String::new()
            };
            out.push(heading("Como continuar"));
            out.push(para(how));
            out.push(if link.is_empty() { code(&[panel_value(ctx)]) } else { link });
        }
    } else if !commands.is_empty() {
        out.push(heading("Como continuar"));
        out.push("<p style=\"margin:0 0 4px\"><b>1) Loop pelo script:</b> rode um dos comandos no seu PC. O primeiro responde pelo <b>número da opção</b>; o segundo aceita texto livre.</p>".into());
        out.push(code(&commands));
        out.push("<p style=\"margin:0 0 8px\"><b>2) Conversa normal (chat do Claude com /orchestrator):</b> não rode o script; responda direto no chat.</p>".into());
        if truthy(ctx.get("monitor_url")) {
            out.push(format!("<p style=\"margin:0 0 8px\"><b>3) Responder no painel:</b> <a href=\"{u}\">{u}</a></p>", u = esc(s(ctx, "monitor_url"))));
        }
    }
    if truthy(ctx.get("summary")) {
        out.push(format!("<p style=\"margin:14px 0 0;color:#6b7280\">Resumo completo: {}</p>", esc(s(ctx, "summary"))));
    }
    out.push("</div>".into());
    (subject, out.concat())
}

/// "short" format: size-limited channels (toast), WhatsApp style.
pub fn build_short(env: &Env, ctx: &Value) -> (String, String) {
    let ev = event(ctx);
    let title = subject_of(ctx);
    if ev == "custom" {
        let body = format!("*{title}*\n{}\n\n*COMO CONTINUAR*\nOrigem: conversa normal. Responda direto no chat do Claude; sem script.", clip(s(ctx, "extra_body"), 600));
        return (title, clip(&body, MAX_SHORT));
    }
    let n = compose_notice(ctx);
    let (closed, total, _) = feature_counts(ctx);
    let compose = |size: usize, with_extra: bool| -> String {
        let mut lines: Vec<String> =
            vec![format!("*{title}*"), n.session.iter().filter(|p| p.0 != "Quando").map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(" | ")];
        if total > 0 && ev != "test" {
            lines.push(format!("Features: {closed} de {total} {}", if delivered(ctx) { "entregues" } else { "verificadas (não entregues)" }));
            let running = string_list(ctx, "running");
            if !running.is_empty() {
                lines.push(format!("Rodando agora: {}", running.join(", ")));
            }
        }
        for (heading, paragraph, items) in &n.sections {
            lines.push(String::new());
            lines.push(format!("*{heading}:*"));
            if let Some(p) = paragraph {
                lines.push(clip(p, size));
            }
            let shown: Vec<&String> = items.iter().take(if with_extra { 4 } else { 2 }).collect();
            lines.extend(shown.iter().map(|i| clip(i, (size / 2).max(40))));
            if items.len() > shown.len() {
                lines.push(format!("(+{})", items.len() - shown.len()));
            }
        }
        if with_extra && truthy(ctx.get("graph_short")) && (ev == "awaiting_user" || ev == "error") {
            lines.extend([String::new(), clip(s(ctx, "graph_short"), 200)]);
        }
        if continue_in_panel(env) {
            if n.how.is_some() {
                lines.extend([String::new(), "*Como continuar*".into(), format!("Acesse o painel: {}", panel_value(ctx))]);
            }
        } else if ["awaiting_user", "error", "test"].contains(&ev) {
            let answer = if truthy(ctx.get("options")) && ev == "awaiting_user" { " --option 1" } else { " --answer \"SUA MENSAGEM\"" };
            let command = format!("{}{}", command_base(ctx), if ev == "error" { "" } else { answer });
            lines.extend([
                String::new(),
                "*Como continuar*".into(),
                format!("1) _Loop pelo script_ (rode no PC{}", if command.contains("--option") { ", troque o 1 pelo número da opção):" } else { "):" }),
                format!("```\n{command}\n```"),
                "2) _Conversa normal no chat do Claude_: responda direto no chat, sem script.".into(),
            ]);
            if truthy(ctx.get("monitor_url")) {
                lines.push(format!("3) _Painel_: responda em {}", s(ctx, "monitor_url")));
            }
        }
        lines.join("\n")
    };
    let mut body = compose(260, true);
    if body.chars().count() > MAX_SHORT {
        body = compose(120, false);
    }
    if body.chars().count() > MAX_SHORT {
        body = compose(40, false);
    }
    if body.chars().count() > MAX_SHORT {
        // never cut the text in the middle: keep the title and the link whole and send the reader to the full summary
        let link = if truthy(ctx.get("summary")) { format!("Resumo completo: {}", s(ctx, "summary")) } else { format!("Acesse o painel: {}", panel_value(ctx)) };
        body = format!(
            "*{title}*\n{}\n\nO texto não cabe aqui.\n{link}",
            clip(&n.session.iter().filter(|p| p.0 != "Quando").map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(" | "), 300)
        );
    }
    (title, body)
}

// ---- channels -----------------------------------------------------------------------------------------------------------------

fn channel_console(subject: &str, body: &str) -> (bool, String) {
    println!("\n*** {subject} ***\n{body}\n");
    (true, "ok".into())
}

fn channel_file(subject: &str, body: &str, work: &str) -> (bool, String) {
    if work.is_empty() {
        return (false, "no work folder".into());
    }
    let target = loop_path(work, "NOTICE.txt");
    let _ = std::fs::create_dir_all(target.parent().expect("has parent"));
    let stamp = time::OffsetDateTime::now_local()
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
        .format(time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]"))
        .unwrap_or_default();
    match std::fs::write(&target, format!("{stamp}\n{subject}\n\n{body}\n")) {
        Ok(()) => (true, "NOTICE.txt".into()),
        Err(e) => (false, e.to_string()),
    }
}

fn channel_toast(subject: &str, body: &str, ctx: &Value) -> (bool, String) {
    if !cfg!(windows) {
        return (false, "toast is Windows-only".into());
    }
    let safe = |t: &str| -> String { t.replace('\'', "").replace('\n', " ").chars().take(200).collect() };
    // the first line of the short body is the title again: the toast shows the first line that is not
    let mut first = body.lines().find(|l| !l.trim().is_empty() && l.trim().trim_matches('*') != subject).unwrap_or("").to_string();
    if truthy(ctx.get("monitor_url")) {
        first = format!("Panel: {}", s(ctx, "monitor_url"));
    }
    let script = format!(
        "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null;$x = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02);$n = $x.GetElementsByTagName('text');$n.Item(0).AppendChild($x.CreateTextNode('{}')) | Out-Null;$n.Item(1).AppendChild($x.CreateTextNode('{}')) | Out-Null;$id = '{{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}}\\WindowsPowerShell\\v1.0\\powershell.exe';[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($id).Show([Windows.UI.Notifications.ToastNotification]::new($x))",
        safe(subject),
        safe(&first)
    );
    let mut toast = std::process::Command::new("powershell");
    toast.args(["-NoProfile", "-NonInteractive", "-Command", &script]).stdin(std::process::Stdio::null());
    match crate::loop_io::hidden(&mut toast).output() {
        Ok(out) if out.status.success() => (true, "ok".into()),
        Ok(out) => (
            false,
            format!(
                "powershell exited with code {}: {}",
                out.status.code().unwrap_or(1),
                String::from_utf8_lossy(&out.stderr).trim().chars().take(100).collect::<String>()
            ),
        ),
        Err(e) => (false, e.to_string().chars().take(100).collect()),
    }
}

fn channel_email(env: &Env, subject: &str, body: &str) -> (bool, String) {
    match email::send(env, &email::recipients(env), subject, body) {
        Some(r) => r,
        None => (false, format!("unknown LOOP_EMAIL_PROVIDER: {}", email::chosen_provider(env))),
    }
}

fn channel_webhook(env: &Env, subject: &str, body: &str, ctx: &Value) -> (bool, String) {
    let url = env.text("LOOP_WEBHOOK_URL");
    let payload = py_json_dumps(&json!({"subject": subject, "body": body, "event": event(ctx), "project": s(ctx, "project")}), true);
    let mut last = String::new();
    for attempt in 0..3 {
        match ureq::post(&url).timeout(Duration::from_secs(30)).set("Content-Type", "application/json").send_string(&payload) {
            Ok(r) => return ((200..300).contains(&r.status()), format!("HTTP {}", r.status())),
            Err(ureq::Error::Status(code, r)) => return (false, format!("HTTP {code} {}", r.into_string().unwrap_or_default().chars().take(200).collect::<String>())),
            Err(e) => {
                last = e.to_string().chars().take(150).collect();
                if attempt < 2 {
                    std::thread::sleep(Duration::from_secs(3));
                }
            }
        }
    }
    (false, format!("no response from the webhook: {last}"))
}

/// What a turn closed, from the ROADMAP text before and after it: `(features that became verified, stages that became fully closed)`.
/// A stage is closed when every feature in it is verified or blocked.
pub fn milestones(before: &str, after: &str) -> (Vec<String>, Vec<String>) {
    use crate::loop_state::parse_roadmap;
    let (old, _) = parse_roadmap(before);
    let (new, _) = parse_roadmap(after);
    let verified: Vec<String> =
        new.iter().filter(|(slug, f)| f.status() == "verified" && old.get(slug).is_some_and(|o| o.status() != "verified")).map(|(slug, _)| slug.clone()).collect();
    let closed_in = |features: &crate::loop_state::Features, stage: &str| {
        let mut any = false;
        let all = features.iter().filter(|(_, f)| f.text("stage") == stage).all(|(_, f)| {
            any = true;
            ["verified", "blocked"].contains(&f.status())
        });
        any && all
    };
    let mut stages: Vec<String> = Vec::new();
    for (_, f) in new.iter() {
        let stage = f.text("stage").to_string();
        if !stage.is_empty() && !stages.contains(&stage) && closed_in(&new, &stage) && !closed_in(&old, &stage) {
            stages.push(stage);
        }
    }
    (verified, stages)
}

/// Milestone notices (a feature verified, a stage closed) are on unless `LOOP_NOTIFY_MILESTONES=off`.
pub fn milestones_enabled(env: &Env) -> bool {
    !matches!(env.text("LOOP_NOTIFY_MILESTONES").trim().to_lowercase().as_str(), "off" | "no" | "false" | "0")
}

/// The channels the notices can use.
pub const CHANNELS: [&str; 5] = ["console", "toast", "file", "email", "webhook"];

/// The variables a channel still needs before it can send (names only).
pub fn missing_config(env: &Env, name: &str) -> Vec<String> {
    let required: Vec<String> = match name {
        "email" => email::required_variables(env),
        "webhook" => vec!["LOOP_WEBHOOK_URL".into()],
        _ => vec![],
    };
    required.into_iter().filter(|v| env.text(v.split(' ').next().unwrap_or("")).is_empty()).collect()
}

/// Sends through the configured channels. Never raises. Returns `[(channel, 'ok: ...' | 'FAILED: ...' | 'SKIPPED: ...')]`.
pub fn dispatch(env: &mut Env, ctx: &Value, work: &str, only: Option<Vec<String>>) -> Vec<(String, String)> {
    load_all(env);
    let full = build(env, ctx);
    let html = build_html(env, ctx);
    let short = build_short(env, ctx);
    let names: Vec<String> = only.filter(|o| !o.is_empty()).unwrap_or_else(|| {
        let listed: Vec<String> = env.text("LOOP_NOTIFY_CHANNELS").split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        if listed.is_empty() {
            vec!["console".into(), "toast".into(), "file".into()]
        } else {
            listed
        }
    });
    let mut results = Vec::new();
    for name in names {
        let style = match name.as_str() {
            "console" | "file" | "webhook" => &full,
            "toast" => &short,
            "email" => &html,
            _ => {
                results.push((name, "FAILED: unknown channel".to_string()));
                continue;
            }
        };
        let missing = missing_config(env, &name);
        if !missing.is_empty() {
            results.push((name.clone(), format!("SKIPPED: not configured ({})", missing.join(", "))));
            continue;
        }
        let (subject, body) = style;
        let (ok, detail) = match name.as_str() {
            "console" => channel_console(subject, body),
            "toast" => channel_toast(subject, body, ctx),
            "file" => channel_file(subject, body, work),
            "email" => channel_email(env, subject, body),
            _ => channel_webhook(env, subject, body, ctx),
        };
        results.push((name, format!("{}{}", if ok { "ok: " } else { "FAILED: " }, detail)));
    }
    results
}

/// `harness notify ...`; returns `(exit_code, stdout, stderr)`.
pub fn run_cli(args: &[String], env: &mut Env) -> (i32, String, String) {
    let opt = |flag: &str| -> String { args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned().unwrap_or_default() };
    load_all(env);
    let only: Vec<String> = opt("--channels").split(',').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect();
    let only = if only.is_empty() { None } else { Some(only) };

    let ctx = if args.iter().any(|a| a == "--test") {
        json!({"event": "test", "project": "test", "branch": "loop/example", "turn": 1, "cost": 0.0, "script": "run_loop.py", "repo": "/path/to/project",
                     "flags": " --wave 1", "worktree": "/path/to/project-worktree-loop", "features": [["example-feature", "verified"], ["other-feature", "pending"]]})
    } else if !opt("--subject").is_empty() && (!opt("--body").is_empty() || !opt("--body-file").is_empty()) {
        let mut text = opt("--body");
        if !opt("--body-file").is_empty() {
            text = std::fs::read(opt("--body-file")).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        }
        json!({"event": "custom", "subject": opt("--subject"), "project": "", "extra_body": clip(&text, 3000)})
    } else {
        return (2, NOTIFY_USAGE.into(), String::new());
    };
    let results = dispatch(env, &ctx, "", only);
    let mut out = String::new();
    for (name, status) in &results {
        out.push_str(&format!("{name:<9} {status}\n"));
    }
    (if results.iter().all(|(_, s)| s.starts_with("ok")) { 0 } else { 1 }, out, String::new())
}

#[allow(dead_code)]
fn _touch(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Env {
        Env::from_pairs([("LOOP_DATA_DIR", "C:\\no-such-dir")])
    }

    #[test]
    fn test_notice_has_the_standard_shape() {
        let ctx = json!({"event": "test", "branch": "loop/example", "turn": 1, "cost": 0.0, "features": [["a", "verified"], ["b", "pending"]]});
        let (subject, body) = build(&env(), &ctx);
        assert_eq!(subject, "[Claude-Maestro] Teste de notificação");
        assert!(body.contains("Detalhes da sessão\n- ID do loop: example\n- Turno: 1\n- Custo atual: US$ 0.00\n- Quando: "));
        assert!(body.contains("Teste\nEste é um aviso de TESTE do loop. Nada precisa ser feito."));
        assert!(!body.contains("Status das features"));
    }

    #[test]
    fn stop_reasons_are_explained() {
        assert!(explain_reason("cost cap reached: US$ 5.00 of 5.00").0.starts_with("O loop foi pausado: o custo total (US$ 5.00) atingiu o limite de US$ 5.00."));
        assert!(explain_reason("turn 3 exceeded 60 min").0.starts_with("O turno 3 passou do limite de 60 minutos"));
        let (p, b) = explain_reason("dirty tree outside .specs/ in repo api before turn 2 (x): M a.txt; ?? b.txt. In the panel use commit");
        assert!(p.contains("no repositório api") && b == vec!["Arquivos com alterações não commitadas:", "a.txt", "b.txt"]);
        let odd = explain_reason("something odd, use --rewind 5 to go back").0;
        assert!(odd.contains("something odd") && !odd.contains("--rewind") && odd.contains("painel"));
        assert!(explain_reason("claude exited with code 1 on turn 4: boom").0.contains("turno 4"));
        assert!(explain_reason("5 consecutive turns without ROADMAP progress").0.contains("5 turnos seguidos"));
    }

    #[test]
    fn clip_never_cuts_a_word() {
        let text = "Primeira frase completa aqui. Segunda frase bem longa que passa do limite configurado para o corte";
        assert_eq!(clip(text, 60), "Primeira frase completa aqui....");
        let words = "alpha beta gamma delta epsilon zeta eta theta";
        let cut = clip(words, 30);
        assert!(cut.ends_with("...") && words.starts_with(cut.trim_end_matches("...")), "{cut}");
        assert!(words.split(' ').any(|w| cut.trim_end_matches("...").ends_with(w)), "{cut}");
        assert_eq!(clip("short", 30), "short");
    }

    fn named_ctx() -> Value {
        json!({"event": "error", "reason": "5 consecutive turns without ROADMAP progress", "branch": "loop/x", "turn": 3, "stage": "2",
               "running": ["b-ui"], "features": [["a-api", "verified"], ["b-ui", "building"], ["c-doc", "pending"]], "commits": ["abc1234 feat: api"],
               "monitor_url": "http://localhost:47801/p/x/"})
    }

    #[test]
    fn notice_names_features_stage_and_does_not_repeat_the_title() {
        let (subject, body) = build(&env(), &named_ctx());
        assert_eq!(subject, "[Claude-Maestro] Execução interrompida com erro: b-ui | etapa 2");
        assert!(!body.contains("Execução interrompida"), "{body}");
        assert!(
            body.contains("- Etapa: 2")
                && body.contains("- a-api: verificada")
                && body.contains("- b-ui: em execução (rodando agora)")
                && body.contains("c-doc: pendente")
        );
        assert!(body.contains("1 de 3 verificadas, ainda não entregues") && !body.contains("Stage ?") && !body.contains("concluídas"));
        assert!(body.contains("Commits recentes\n- abc1234 feat: api"));
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let mut e = env();
        dispatch(&mut e, &named_ctx(), &work, Some(vec!["file".into()]));
        let file = std::fs::read_to_string(loop_path(&work, "NOTICE.txt")).unwrap();
        assert_eq!(file.matches("Execução interrompida com erro").count(), 1, "{file}");
    }

    #[test]
    fn delivered_and_verified_use_distinct_words() {
        let mut ctx = named_ctx();
        ctx["event"] = json!("done");
        ctx["push"] = json!("done: origin loop/x");
        let (_, body) = build(&env(), &ctx);
        assert!(body.contains("1 de 3 entregues") && body.contains("a branch foi enviada (entregue)"));
        ctx["push"] = json!("not done");
        let (_, body) = build(&env(), &ctx);
        assert!(body.contains("nada foi entregue ainda"));
    }

    #[test]
    fn short_format_keeps_the_panel_link_whole() {
        let mut ctx = named_ctx();
        ctx["reason"] = json!("x".repeat(3000) + " tail");
        let (_, short) = build_short(&env(), &ctx);
        assert!(short.chars().count() <= 1000 && short.contains("http://localhost:47801/p/x/"), "{short}");
    }

    #[test]
    fn milestones_name_new_verified_features_and_closed_stages() {
        let before = "## Stage 1\n- [a] status=building paths=x\n- [b] status=verified paths=y\n## Etapa 2\n- [c] status=pending paths=z\n";
        let after = "## Stage 1\n- [a] status=verified paths=x\n- [b] status=verified paths=y\n## Etapa 2\n- [c] status=pending paths=z\n";
        assert_eq!(milestones(before, after), (vec!["a".to_string()], vec!["1".to_string()]));
        assert_eq!(milestones(after, after), (vec![], vec![]));
        let ctx = json!({"event": "stage_closed", "verified_now": ["a"], "stage_closed": ["1"], "features": [["a", "verified"]]});
        let (subject, body) = build(&env(), &ctx);
        assert_eq!(subject, "[Claude-Maestro] Etapa concluída: a");
        assert!(body.contains("a: verificada (ainda não entregue)") && body.contains("Etapa concluída: 1"));
        assert!(!milestones_enabled(&Env::from_pairs([("LOOP_NOTIFY_MILESTONES", "off")])));
    }

    #[test]
    fn awaiting_user_lists_options_and_the_panel() {
        let ctx = json!({"event": "awaiting_user", "reason": "Which DB?", "options": ["pg", "mysql"], "monitor_url": "http://localhost:47801/p/x/"});
        let (_, body) = build(&env(), &ctx);
        assert!(body.contains("Decisão necessária\nWhich DB?\n- Opção 1: pg\n- Opção 2: mysql"));
        assert!(body.ends_with("Como continuar\nPara resolver, responder, retomar ou aprovar, acesse o painel de controle:\nhttp://localhost:47801/p/x/"));
        let (_, short) = build_short(&env(), &ctx);
        assert!(short.contains("Acesse o painel: http://localhost:47801/p/x/") && short.chars().count() <= 1000);
    }

    #[test]
    fn html_is_escaped() {
        let ctx = json!({"event": "custom", "subject": "S", "extra_body": "a < b & \"c\""});
        let (_, html) = build_html(&env(), &ctx);
        assert!(html.contains("a &lt; b &amp; &quot;c&quot;"));
    }

    #[test]
    fn file_channel_writes_notice() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().display().to_string();
        let mut e = env();
        let res = dispatch(&mut e, &json!({"event": "test"}), &work, Some(vec!["file".into(), "nope".into(), "email".into()]));
        assert_eq!(res[0], ("file".to_string(), "ok: NOTICE.txt".to_string()));
        assert_eq!(res[1].1, "FAILED: unknown channel");
        assert!(res[2].1.starts_with("SKIPPED: not configured (LOOP_NOTIFY_TO, LOOP_SMTP_HOST, LOOP_SMTP_FROM)"));
        assert!(loop_path(&work, "NOTICE.txt").is_file());
    }
}
