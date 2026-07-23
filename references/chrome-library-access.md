# Google Scholar library access through Chrome DevTools MCP

Read this file only when the user has explicitly enabled
`use_google_scholar_library_access` in a plan. This option also requires
`include_paywalled: true`.

## Keep installation outside this repository

Prefer an already installed, user-configured Chrome DevTools MCP server. Its canonical package and
server name are `chrome-devtools-mcp` and `chrome-devtools`; the user may call it
“chrome-dev-mcp.” If it is absent, install it only through the authorized user-scope procedure
below. Never add Node/npm files, `.prototools`, MCP manifests, or Chrome settings to this
repository.

If the server is unavailable, first give the user:

- the official setup guide: <https://developer.chrome.com/docs/devtools/agents/get-started>;
- the official configuration guide for connecting an existing Chrome session:
  <https://developer.chrome.com/docs/devtools/agents/get-started/configuration>;
- the requirements: current stable Chrome plus Node.js LTS and npm, managed outside this
  repository.

Explain exactly which user-level toolchain and MCP configuration would change, that package
retrieval needs network access, and that attaching to an authenticated browser exposes that
profile's page contents to the agent. Ask for explicit authorization before installing anything.
If the user declines or does not answer, use the normal manual PDF handoff.

After authorization, inspect the host runtime's authoritative MCP setup mechanism and use it at
user scope; do not invent or commit a repository-local manifest. Manage any required Node.js LTS
and npm installation through `proto`, never a system-global package manager, and request separate
approval if installing those tools was not covered by the user's authorization. For Codex, explain
that the following example changes `~/.prototools` and the user's Codex MCP configuration, then run
it only if those exact user-level changes were authorized:

```bash
proto install node lts --pin user
proto install npm --pin user
codex mcp add chrome-devtools -- \
  proto exec node npm -- \
  npx -y chrome-devtools-mcp@latest \
  --autoConnect --no-usage-statistics --no-performance-crux
```

For another runtime, adapt its official user-scope MCP command so `npx` runs inside the same
proto-provided Node/npm environment. After changing MCP configuration, tell the user what changed,
ask them to restart or reload the runtime, and verify that `chrome-devtools` tools are visible
before enabling the option. Never claim the browser workflow is available merely because an
install command returned successfully.

## Obtain explicit browser consent

Before attaching to Chrome, explain that the MCP can read and act on pages in the connected
profile, including authenticated content. Ask the user to:

1. use a dedicated Chrome profile with no unrelated sensitive tabs;
2. sign in to Google and configure their institutional affiliation under Google Scholar’s
   **Settings → Library links** themselves;
3. complete any library proxy, VPN, publisher, 2FA, consent, or CAPTCHA interaction themselves;
4. confirm that their subscription terms permit the download, local preservation, and configured
   NVIDIA processing;
5. enable Chrome remote debugging and approve the MCP connection themselves when their chosen
   setup requires it.

Never read, request, export, or replay passwords, session cookies, authorization headers, proxy
tokens, or 2FA codes. Never inspect unrelated tabs or account settings. Do not use an incognito or
fresh automated profile that lacks the user-established library affiliation.

## Resolve one DOI at a time

Use this path only for an item emitted in `manual_downloads` with a DOI and a
`google_scholar_query_url`:

1. Select the dedicated, user-approved Chrome page and navigate to the supplied Scholar URL, or
   enter the exact DOI in Scholar’s visible search box.
2. Process one DOI per explicit pending item. Do not bulk-query, scrape result lists, parallelize
   Scholar requests, or retry a blocked request. Google Scholar does not provide bulk access and
   directs automated software to respect its robots policy.
3. Match the result’s DOI, title, and authors to the pending work. If identity is ambiguous, stop
   and ask the user.
4. Prefer a visible link labeled with the configured library, then a legitimate `[PDF]`/`[HTML]`
   access link, then **All versions**, as described by Google Scholar’s official help:
   <https://scholar.google.com/intl/us/scholar/help.html>.
5. Follow only visible Scholar, library-resolver, repository, or publisher controls. Never call
   hidden endpoints, replay network requests, extract cookies, evade metering, or bypass a paywall.
6. If a login, 2FA, consent screen, CAPTCHA, bot block, purchase prompt, or license warning appears,
   stop and give control to the user. Continue only after they confirm completion and access.
7. Use the page’s visible download control. If Chrome saves to its configured download directory,
   identify only that newly completed download and move or rename it to the exact `destination`
   from the handoff. Do not scan the user’s home or downloads directory broadly. If the MCP or
   filesystem sandbox cannot identify or move it safely, tell the user the exact source filename
   and destination and let them perform the move.
8. Rerun `litmine download --workspace <workspace>`. The CLI, not the browser, validates the PDF,
   size, path, and checksum. Verify the PDF identity before `render` and `ingest`.

Close the Scholar/library tabs or disconnect from the browser when the bounded handoff is done.
Fall back to the normal manual publisher-link workflow for any failure; never weaken access or
identity checks to make browser automation succeed.
