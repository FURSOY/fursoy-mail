# FURSOY Mail website

Dependency-free static website for FURSOY Mail.

## Routes

- `/` — product overview and Windows download
- `/download/` — resolves and downloads the latest GitHub release installer
- `/privacy/` — public Google user-data privacy policy
- `/terms/` — public software license and legal notices

## Local preview

Serve the `website` directory with any static file server.

## Cloudflare Workers Builds

- Production branch: `main`
- Worker name: `fursoy-mail`
- Root directory: `website`
- Build command: `exit 0`
- Deploy command: `npx wrangler deploy`

The Worker configuration is stored in `wrangler.jsonc`. No project environment
variables or package installation are required.
