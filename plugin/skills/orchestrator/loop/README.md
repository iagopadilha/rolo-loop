# The loop runtime lives in the `harness` executable

The unsupervised loop engine (`harness loop`, `harness state`, `harness notify`, `harness deliver`, `harness summary`, `harness rules`,
`harness audit`, `harness graph`, `harness scale`) and its tests are part of the rolo-loop application, next to this plugin in the
same repository (`crates/engine-state`).

Run everything through the CLI (`<repo>` = the project folder):

```
harness check <repo>                 # preflight: prints READY and the command, creates nothing
harness loop <repo> --wave 1         # the loop (all harness loop flags are accepted)
harness serve --project <repo>       # the web panel
harness state|notify|summary|deliver|rules|audit|graph|classify|scale ...   # the engine commands
harness install                      # lets the skill find the app (~/.loop-harness/config.json)
```

See `docs/HOW_TO_USE.md` in the repository.
