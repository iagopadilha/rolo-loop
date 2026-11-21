// SPDX-License-Identifier: MIT
//! The demo story (data only): project "acme-checkout", its 9 features in 3 stages and the turn plan the player applies.
//!
//! A turn plan is a list of steps; each step is a list of actions. One step is what the animator applies per tick. The agent of a feature action is
//! the one the panel infers from the feature status (spec-writer, spec-reviewer, executor-<kind>, verifier), so the live log lines land on the agent
//! card the panel shows.

use serde_json::{json, Value};

pub const PROJECT_NAME: &str = "acme-checkout";
pub const LOOP_BRANCH: &str = "loop/acme-checkout";
pub const INTEGRATION_BRANCH: &str = "developer";
pub const SCAFFOLD_BRANCH: &str = "main";
pub const WAVE: u32 = 2;
pub const MAX_COST_USD: u32 = 15;
pub const TURN_TIMEOUT_MINUTES: u32 = 45;
/// Turns that already happened when the demo opens; the animator plays the rest.
pub const SEEDED_TURNS: usize = 2;
pub const GOAL: &str = "Ship a card checkout for the Acme store: payment core, cart and coupons, then invoices and receipts.";

pub struct Stage {
    pub n: u32,
    pub title: &'static str,
    pub description: &'static str,
}

pub const STAGES: [Stage; 3] = [
    Stage { n: 1, title: "Foundations", description: "Payment core, audit trail and rate limiting that every later stage builds on." },
    Stage { n: 2, title: "Checkout flow", description: "Cart, coupons and the checkout screens the customer actually sees." },
    Stage { n: 3, title: "After the purchase", description: "Invoices, receipts and the back-office view of every order." },
];

pub struct Feature {
    pub slug: &'static str,
    pub stage: u32,
    pub title: &'static str,
    pub kind: &'static str,
    pub scope: &'static str,
    pub paths: &'static str,
    pub deps: &'static str,
    pub prefix: &'static str,
    pub file: &'static str,
    pub goal: &'static str,
    pub requirements: &'static [&'static str],
}

pub const FEATURES: [Feature; 9] = [
    Feature {
        slug: "payments-api",
        stage: 1,
        title: "Payments API",
        kind: "back",
        scope: "Medium",
        paths: "services/payments-api",
        deps: "",
        prefix: "PAY",
        file: "services/payments-api/handlers.py",
        goal: "Authorize, capture and refund card payments behind one idempotent REST API, with provider webhooks verified and replayed safely.",
        requirements: &["authorize and capture with an idempotency key", "partial and full refunds", "verify webhook signatures", "replay failed webhooks with backoff"],
    },
    Feature {
        slug: "audit-log",
        stage: 1,
        title: "Audit log",
        kind: "back",
        scope: "Small",
        paths: "services/audit-log",
        deps: "",
        prefix: "AUD",
        file: "services/audit-log/writer.py",
        goal: "Append-only record of every money movement and admin action, queryable by order and by actor.",
        requirements: &["append-only writes", "query by order id", "query by actor"],
    },
    Feature {
        slug: "rate-limiter",
        stage: 1,
        title: "Rate limiter",
        kind: "generic",
        scope: "Small",
        paths: "infra/rate-limiter",
        deps: "",
        prefix: "RTL",
        file: "infra/rate-limiter/limits.yaml",
        goal: "Token-bucket limits per API key at the edge, so a single client cannot starve checkout.",
        requirements: &["token bucket per API key", "burst allowance", "429 with Retry-After"],
    },
    Feature {
        slug: "cart-service",
        stage: 2,
        title: "Cart service",
        kind: "back",
        scope: "Medium",
        paths: "services/cart-service",
        deps: "payments-api",
        prefix: "CRT",
        file: "services/cart-service/cart.py",
        goal: "Server-side cart with price snapshots, stock reservation for 15 minutes and a single total the payment step trusts.",
        requirements: &["add, update and remove items", "price snapshot per line", "reserve stock for 15 minutes", "one signed total"],
    },
    Feature {
        slug: "coupon-rules",
        stage: 2,
        title: "Coupon rules",
        kind: "back",
        scope: "Small",
        paths: "services/coupon-rules",
        deps: "cart-service",
        prefix: "CPN",
        file: "services/coupon-rules/rules.py",
        goal: "Percentage and fixed coupons with expiry, minimum order and one-use-per-customer rules, applied to the cart total.",
        requirements: &["percentage and fixed coupons", "expiry and minimum order", "one use per customer"],
    },
    Feature {
        slug: "checkout-ui",
        stage: 2,
        title: "Checkout UI",
        kind: "front",
        scope: "Large",
        paths: "web/checkout-ui",
        deps: "cart-service,dir:web/shared",
        prefix: "CUI",
        file: "web/checkout-ui/CheckoutPage.tsx",
        goal: "Three-step checkout (cart, address, payment) with inline validation, a coupon field and a clear error on declined cards.",
        requirements: &["cart, address and payment steps", "inline validation", "coupon field", "declined-card message", "keyboard only flow"],
    },
    Feature {
        slug: "invoice-pdf",
        stage: 3,
        title: "Invoice PDF",
        kind: "back",
        scope: "Medium",
        paths: "services/invoice-pdf",
        deps: "payments-api,audit-log",
        prefix: "INV",
        file: "services/invoice-pdf/render.py",
        goal: "Numbered PDF invoice per captured payment, rendered from a template and stored next to the order.",
        requirements: &["sequential invoice numbers", "PDF from a template", "store with the order"],
    },
    Feature {
        slug: "email-receipts",
        stage: 3,
        title: "Email receipts",
        kind: "back",
        scope: "Small",
        paths: "services/email-receipts",
        deps: "invoice-pdf",
        prefix: "EML",
        file: "services/email-receipts/send.py",
        goal: "Receipt email with the invoice attached, sent once per order and retried when the mail provider is down.",
        requirements: &["one receipt per order", "invoice attached", "retry on provider failure"],
    },
    Feature {
        slug: "admin-dashboard",
        stage: 3,
        title: "Admin dashboard",
        kind: "front",
        scope: "Medium",
        paths: "web/admin-dashboard",
        deps: "audit-log,dir:web/shared",
        prefix: "ADM",
        file: "web/admin-dashboard/OrdersPage.tsx",
        goal: "Back-office list of orders with payment status, refunds and the audit trail of each order.",
        requirements: &["orders list with filters", "refund button with confirmation", "audit trail per order"],
    },
];

pub fn feature(slug: &str) -> &'static Feature {
    FEATURES.iter().find(|f| f.slug == slug).expect("a feature of the story")
}

pub const SCAFFOLD_FOLDERS: [&str; 12] = [
    "services/payments-api",
    "services/audit-log",
    "services/cart-service",
    "services/coupon-rules",
    "services/invoice-pdf",
    "services/email-receipts",
    "infra/rate-limiter",
    "infra/prod",
    "web/shared",
    "web/checkout-ui",
    "web/admin-dashboard",
    "migrations",
];

pub const PROJECT_TEXT: &str = "# acme-checkout

<!-- Context for the loop's subagents (spec-writer, executors, verifier), like a CLAUDE.md for the loop. Keep it short
     (~300 words). It does not repeat what other files already say. -->

**Vision:** Card checkout for the Acme store, built as small services behind one gateway: payments, cart, coupons,
invoices and receipts, plus a checkout screen for customers and a back-office dashboard.

## Where to look

- Stack, architecture, conventions, structure, tests, integrations, concerns: `docs/codebase/` (read only the document the task needs; it is a guide, the code wins).
- Limits and \"never X\": `RULES.md`.
- Now/next, stage, open questions: `STATE.md`.
- Roadmap: `ROADMAP.md`.
- Flows to validate: `FLOWS.md`.
- Test credentials (names only): `TEST-ENVIRONMENT.md`.
";

pub const RULES_TEXT: &str = "# Project rules

- Never edit `infra/prod/`: production infrastructure is applied by the platform team.
- Never write migrations from a feature: `migrations/` is reviewed by hand (blocked by PERMISSIONS.json).
- Prices and totals in integer cents.
";

pub fn permissions() -> Value {
    json!({"deny_path_regex": ["(^|/)migrations/"]})
}

pub const STATE_HEADER: &str = "# State

Loop on branch `loop/acme-checkout`, wave 2, cost cap US$ 15.

## Turn log
";

pub const VALIDATE_LINES: [&str; 2] = [
    "- [payments-api] webhook retries: the provider allows 3 or 5 | chose: 5 with exponential backoff | revert: set WEBHOOK_MAX_RETRIES=3",
    "- [audit-log] retention period not stated in the spec | chose: 400 days (one fiscal year plus margin) | revert: change AUDIT_RETENTION_DAYS",
];

pub const SEEDED_QUESTION: &str = "payments-api: which payment provider mode should the tests use?";
pub const SEEDED_ANSWER: &str = "the provider's public test mode, with fake cards only";

pub const QUESTION_REASON: &str = "checkout-ui: where should the coupon code field live? The spec allows both and it changes which service validates the code first.";
pub const QUESTION_OPTIONS: [&str; 2] = ["On the cart step (recommended)", "On the payment step"];
pub const QUESTION_TO_CHECK: [&str; 1] = ["the coupon field position in web/checkout-ui after the next turn"];
pub const QUESTION_ANSWER: &str = "On the cart step (recommended)";
pub const QUESTION_HOLD_SECONDS: f64 = 10.0;

pub fn stage_closing(number: u32) -> Vec<(&'static str, &'static str)> {
    match number {
        1 => vec![
            ("Delivered", "payments-api, audit-log and rate-limiter verified, one commit each."),
            ("User decisions", "Tests use the provider's public test mode."),
            ("Lessons", "The guard stopped an edit to infra/prod/terraform.tfvars: edge limits stay in infra/rate-limiter."),
            ("Unverified", "Webhook replay under real provider latency."),
            ("Blocked", "None."),
            ("For the next stage", "cart-service can start: payments-api is verified."),
        ],
        2 => vec![
            ("Delivered", "cart-service, coupon-rules and checkout-ui verified."),
            ("User decisions", "Coupon field on the cart step."),
            ("Lessons", "Ask early when the spec allows two layouts: one question saved a rework turn."),
            ("Unverified", "Checkout on very small screens."),
            ("Blocked", "None."),
            ("For the next stage", "invoice-pdf and admin-dashboard can run together (disjoint paths)."),
        ],
        _ => vec![
            ("Delivered", "invoice-pdf, email-receipts and admin-dashboard verified."),
            ("User decisions", "None in this stage."),
            ("Lessons", "The guard blocked a `git commit -a` from an executor: only the orchestrator commits."),
            ("Unverified", "Receipt rendering in older mail clients."),
            ("Blocked", "None."),
            ("For the next stage", "Deliver the branch from the panel (Act: deliver)."),
        ],
    }
}

pub const TURN_COSTS_USD: [f64; 10] = [0.38, 1.12, 0.74, 0.52, 0.88, 0.95, 0.67, 0.49, 1.21, 0.58];

pub const GUARD_HOOK_COMMAND: &str = "\"${CLAUDE_PLUGIN_ROOT}/bin/guard.exe\" --profile auto";
pub const GUARD_MARKER: &str = "BLOCKED by the orchestrator guard";
pub const GUARD_ADVICE: &str = "Do not try to work around it. Return STATUS: NEEDS_CONFIRM with the reason; the orchestrator takes it to the user.";

#[derive(Clone, Debug)]
pub enum Action {
    Orchestrator { text: String },
    OrchestratorTool { tool: &'static str, input: Value, result: String },
    Status { slug: &'static str, status: &'static str, note: Option<&'static str> },
    AgentTool { slug: &'static str, tool: &'static str, input: Value, result: String },
    AgentText { slug: &'static str, text: String },
    AgentReturn { slug: &'static str, text: String },
    GuardBlock { slug: &'static str, tool: &'static str, input: Value, rule: &'static str, reason: String, profile: &'static str },
    Spec { slug: &'static str },
    Commit { slug: &'static str },
    StageClosing { stage: u32 },
    EndTurn { status: &'static str, result: String, reason: String, next: String, options: Vec<String>, to_check: Vec<String> },
    WaitForAnswer { question: String, answer: String, hold_seconds: f64 },
}

pub type Step = Vec<Action>;
pub type TurnPlan = (usize, Vec<Step>);

fn orchestrator(text: impl Into<String>) -> Action {
    Action::Orchestrator { text: text.into() }
}

fn orchestrator_tool(tool: &'static str, input: Value, result: &str) -> Action {
    Action::OrchestratorTool { tool, input, result: result.into() }
}

fn status(slug: &'static str, new_status: &'static str) -> Action {
    Action::Status { slug, status: new_status, note: None }
}

fn status_note(slug: &'static str, new_status: &'static str, note: &'static str) -> Action {
    Action::Status { slug, status: new_status, note: Some(note) }
}

fn agent_tool(slug: &'static str, tool: &'static str, input: Value, result: impl Into<String>) -> Action {
    Action::AgentTool { slug, tool, input, result: result.into() }
}

fn agent_text(slug: &'static str, text: impl Into<String>) -> Action {
    Action::AgentText { slug, text: text.into() }
}

fn agent_return(slug: &'static str, text: impl Into<String>) -> Action {
    Action::AgentReturn { slug, text: text.into() }
}

fn guard_block(slug: &'static str, tool: &'static str, input: Value, rule: &'static str, reason: impl Into<String>) -> Action {
    Action::GuardBlock { slug, tool, input, rule, reason: reason.into(), profile: "executor" }
}

fn end_turn(turn_status: &'static str, result: &str, reason: &str, next_step: &str, options: &[&str], to_check: &[&str]) -> Action {
    Action::EndTurn {
        status: turn_status,
        result: result.into(),
        reason: reason.into(),
        next: next_step.into(),
        options: options.iter().map(|s| s.to_string()).collect(),
        to_check: to_check.iter().map(|s| s.to_string()).collect(),
    }
}

pub fn spec_file_path(slug: &str) -> String {
    format!(".specs/features/{slug}/spec.md")
}

pub fn stage_slugs(stage_number: u32) -> Vec<&'static str> {
    FEATURES.iter().filter(|f| f.stage == stage_number).map(|f| f.slug).collect()
}

/// 3 steps: spec-writers start, read the code, write the specs (pending -> speccing -> specced).
fn build_spec_steps(slugs: &[&'static str]) -> Vec<Step> {
    let mut first = vec![orchestrator(format!("Dispatching {} spec-writer(s) in parallel: {}.", slugs.len(), slugs.join(", ")))];
    let (mut second, mut third) = (vec![], vec![]);
    for slug in slugs {
        let item = feature(slug);
        first.push(orchestrator_tool("Agent", json!({"description": format!("spec {slug}"), "subagent_type": "spec-writer"}), "agent started"));
        first.push(status(slug, "speccing"));
        first.push(agent_tool(slug, "Read", json!({"file_path": ".specs/project/PROJECT.md"}), "# acme-checkout ..."));
        second.push(agent_tool(slug, "Grep", json!({"pattern": "def |class ", "path": item.paths}), "3 files"));
        second.push(agent_text(slug, "The folder only has a README; the spec defines the module from scratch."));
        third.push(Action::Spec { slug });
        third.push(agent_tool(slug, "Write", json!({"file_path": spec_file_path(slug)}), "File created successfully"));
        third.push(agent_return(slug, format!("STATUS: DONE\nSUMMARY: spec with {} requirements, scope {}.", item.requirements.len(), item.scope)));
        third.push(status(slug, "specced"));
    }
    vec![first, second, third]
}

/// 1 step: the spec-reviewer approves each spec (specced -> approved).
fn build_gate_step(slugs: &[&'static str]) -> Vec<Step> {
    let mut step = vec![orchestrator(format!("Spec gate for {}.", slugs.join(", ")))];
    for slug in slugs {
        let item = feature(slug);
        step.push(agent_tool(slug, "Read", json!({"file_path": spec_file_path(slug)}), format!("# {} ...", item.title)));
        step.push(agent_return(slug, format!("VERDICT: APPROVED\nSUMMARY: {} requirements are testable and inside {}.", item.requirements.len(), item.paths)));
        step.push(status(slug, "approved"));
    }
    vec![step]
}

fn test_command(item: &Feature) -> String {
    match item.kind {
        "front" => format!("npm test --prefix {} -- --run", item.paths),
        "generic" => "python -m pytest infra/rate-limiter -q".to_string(),
        _ => format!("python -m pytest {} -q", item.paths),
    }
}

fn lint_command(item: &Feature) -> String {
    if item.kind == "front" {
        format!("npm run lint --prefix {}", item.paths)
    } else {
        format!("ruff check {}", item.paths)
    }
}

fn path_scope_reason(paths: &str) -> String {
    format!("outside the feature's PATHS ({paths}). Need a file outside the scope? Return NEEDS_CONFIRM")
}

/// 4 steps (+1 per guard block): executors implement, a verifier checks, the quality gate reviews, the orchestrator commits (approved -> verified). `blocks` = a guard block
/// per slug, inserted after the executor's first edit.
fn build_execution_steps(slugs: &[&'static str], blocks: &[(&'static str, Action)]) -> Vec<Step> {
    let together = if slugs.len() > 1 { "together (disjoint paths, no dependency between them)" } else { "alone" };
    let mut start = vec![orchestrator(format!("Execution wave: {} {together}.", slugs.join(", ")))];
    let (mut write, mut verify, mut close) = (vec![], vec![], vec![]);
    for slug in slugs {
        let item = feature(slug);
        start.push(orchestrator_tool("Agent", json!({"description": format!("implement {slug}"), "subagent_type": format!("executor-{}", item.kind)}), "agent started"));
        start.push(status(slug, "building"));
        start.push(agent_tool(slug, "Read", json!({"file_path": spec_file_path(slug)}), format!("# {} ...", item.title)));
        let passed = format!("{} passed", item.requirements.len() * 3);
        write.push(agent_tool(slug, "Write", json!({"file_path": item.file}), "File created successfully"));
        write.push(agent_tool(slug, "Bash", json!({"command": test_command(item)}), passed.clone()));
        verify.push(agent_return(slug, format!("STATUS: DONE\nSUMMARY: {} implemented inside {}; tests added.", item.title, item.paths)));
        verify.push(status_note(slug, "building", "verifying"));
        verify.push(agent_tool(slug, "Bash", json!({"command": test_command(item)}), passed));
        verify.push(agent_tool(slug, "Bash", json!({"command": lint_command(item)}), "no issues"));
        close.push(agent_return(slug, format!("VERDICT: PASS\nSUMMARY: tests, lint and typecheck green for {slug}.")));
        close.push(status_note(slug, "building", "quality review"));
        close.push(agent_tool(slug, "Bash", json!({"command": format!("git diff --stat -- {}", item.paths)}), "2 files changed"));
        close.push(agent_return(
            slug,
            format!("VERDICT: APPROVED\nFINDINGS: none\nNOTES: {} matches its spec; no secret, no giant file, names follow the project.", item.title),
        ));
        close.push(Action::Commit { slug });
        close.push(orchestrator_tool("Bash", json!({"command": format!("git commit -m \"feat({slug}): {}\"", item.title.to_lowercase())}), "1 file changed"));
    }
    let mut steps = vec![start, write];
    for slug in slugs {
        if let Some((_, block)) = blocks.iter().find(|(s, _)| s == slug) {
            steps.push(vec![block.clone(), agent_text(slug, format!("The guard blocked that edit (outside my scope). Staying inside {}.", feature(slug).paths))]);
        }
    }
    steps.push(verify);
    steps.push(close);
    steps
}

/// `[(turn number, [steps])]`: turns 1-2 are seeded instantly, the rest are animated.
pub fn build_turn_plans() -> Vec<TurnPlan> {
    let (stage_1, stage_2, stage_3) = (stage_slugs(1), stage_slugs(2), stage_slugs(3));
    let migration_block =
        guard_block("audit-log", "Write", json!({"file_path": "migrations/0007_drop_users.sql"}), "project-deny", "path forbidden by the project: (^|/)migrations/");
    let terraform_block = guard_block("rate-limiter", "Edit", json!({"file_path": "infra/prod/terraform.tfvars"}), "path-scope", path_scope_reason("infra/rate-limiter"));
    let commit_block =
        guard_block("admin-dashboard", "Bash", json!({"command": "git commit -am \"wip dashboard\""}), "git-write", "git commit: write git is the orchestrator's only");
    let mut plans: Vec<TurnPlan> = Vec::new();

    let mut turn_1 = build_spec_steps(&stage_1);
    turn_1.last_mut().unwrap().push(end_turn("continue", "Stage 1: specs written for payments-api, audit-log and rate-limiter.", "", "spec gate", &[], &[]));
    plans.push((1, turn_1));

    let mut turn_2 = build_gate_step(&stage_1);
    turn_2.extend(build_execution_steps(&["payments-api", "audit-log"], &[("audit-log", migration_block)]));
    turn_2.last_mut().unwrap().push(end_turn(
        "continue",
        "payments-api and audit-log verified (2 commits); the guard blocked a migration write.",
        "",
        "rate-limiter",
        &[],
        &[],
    ));
    plans.push((2, turn_2));

    let mut turn_3 = build_execution_steps(&["rate-limiter"], &[("rate-limiter", terraform_block)]);
    turn_3.last_mut().unwrap().extend([
        Action::StageClosing { stage: 1 },
        end_turn("continue", "Stage 1 closed: rate-limiter verified; the guard kept the executor out of infra/prod.", "", "stage 2 specs", &[], &[]),
    ]);
    plans.push((3, turn_3));

    let mut turn_4 = build_spec_steps(&stage_2);
    turn_4.last_mut().unwrap().push(end_turn("continue", "Stage 2: specs written for cart-service, coupon-rules and checkout-ui.", "", "spec gate", &[], &[]));
    plans.push((4, turn_4));

    let mut turn_5 = build_gate_step(&stage_2);
    turn_5.extend(build_execution_steps(&["cart-service"], &[]));
    turn_5.last_mut().unwrap().push(end_turn(
        "continue",
        "cart-service verified; coupon-rules and checkout-ui were waiting on it.",
        "",
        "coupon-rules + checkout-ui",
        &[],
        &[],
    ));
    plans.push((5, turn_5));

    let mut turn_6 = build_execution_steps(&["coupon-rules"], &[]);
    turn_6[1].extend([
        orchestrator_tool("Agent", json!({"description": "implement checkout-ui", "subagent_type": "executor-front"}), "agent started"),
        status("checkout-ui", "building"),
        agent_tool("checkout-ui", "Read", json!({"file_path": spec_file_path("checkout-ui")}), "# Checkout UI ..."),
    ]);
    turn_6[2].extend([
        agent_text("checkout-ui", "The spec allows the coupon field on the cart step or on the payment step; asking."),
        agent_return("checkout-ui", "STATUS: NEEDS_CONFIRM\nSUMMARY: coupon field position is a product decision."),
        status("checkout-ui", "approved"),
    ]);
    turn_6.last_mut().unwrap().push(end_turn(
        "awaiting_user",
        "coupon-rules verified; checkout-ui needs a decision from you.",
        QUESTION_REASON,
        "",
        &QUESTION_OPTIONS,
        &QUESTION_TO_CHECK,
    ));
    turn_6.push(vec![Action::WaitForAnswer { question: QUESTION_REASON.into(), answer: QUESTION_ANSWER.into(), hold_seconds: QUESTION_HOLD_SECONDS }]);
    plans.push((6, turn_6));

    let mut turn_7 = build_execution_steps(&["checkout-ui"], &[]);
    turn_7.last_mut().unwrap().extend([
        Action::StageClosing { stage: 2 },
        end_turn("continue", "Stage 2 closed: checkout-ui verified with the coupon field on the cart step.", "", "stage 3 specs", &[], &[]),
    ]);
    plans.push((7, turn_7));

    let mut turn_8 = build_spec_steps(&stage_3);
    turn_8.last_mut().unwrap().push(end_turn("continue", "Stage 3: specs written for invoice-pdf, email-receipts and admin-dashboard.", "", "spec gate", &[], &[]));
    plans.push((8, turn_8));

    let mut turn_9 = build_gate_step(&stage_3);
    turn_9.extend(build_execution_steps(&["invoice-pdf", "admin-dashboard"], &[("admin-dashboard", commit_block)]));
    turn_9.last_mut().unwrap().push(end_turn(
        "continue",
        "invoice-pdf and admin-dashboard verified; the guard blocked a git commit from an executor.",
        "",
        "email-receipts",
        &[],
        &[],
    ));
    plans.push((9, turn_9));

    let mut turn_10 = build_execution_steps(&["email-receipts"], &[]);
    turn_10.last_mut().unwrap().extend([
        Action::StageClosing { stage: 3 },
        end_turn("done", "All 9 features verified. Deliver the branch from the panel (Act: deliver).", "all features verified (9/9)", "", &[], &[]),
    ]);
    plans.push((10, turn_10));
    plans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_story_has_ten_turns_and_every_feature_gets_verified() {
        let plans = build_turn_plans();
        assert_eq!(plans.len(), 10);
        let commits: Vec<&str> =
            plans.iter().flat_map(|(_, steps)| steps.iter().flatten()).filter_map(|a| if let Action::Commit { slug } = a { Some(*slug) } else { None }).collect();
        assert_eq!(commits.len(), 9);
        for item in &FEATURES {
            assert!(commits.contains(&item.slug), "{} is never committed", item.slug);
        }
        assert!(plans.iter().all(|(_, steps)| steps.iter().rev().flatten().any(|a| matches!(a, Action::EndTurn { .. } | Action::WaitForAnswer { .. }))));
    }
}
