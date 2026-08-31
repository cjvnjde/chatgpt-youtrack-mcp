# sbx Environment

You are in an isolated Docker sandbox, not on the host.

- HTTPS is limited to `omp.sh`, `registry.npmjs.org`, `api.github.com`, `github.com`, and
  `release-assets.githubusercontent.com`. Ask the user to update host-side `sbx policy` for others.
- The sandbox has its own `localhost`. Use `host.docker.internal` for host services.
- Services must listen on `0.0.0.0` or `::`; the user exposes them with host-side `sbx ports`.
- Keep durable work in the mounted workspace.
- Put persistent environment settings, but not shell completions, in
  `/etc/sandbox-persistent.sh`. Open a fresh login shell after installing commands.
- Treat policy or isolation failures as sandbox constraints and state any required host action.
