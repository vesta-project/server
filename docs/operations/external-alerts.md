# External operational alerts

External delivery is an opt-in projection of the existing automation notification
inbox. It does not create another scheduler or change existing workflow authority.
No endpoint can accept a caller-supplied delivery URL. An OS administrator configures
destinations in protected `host.toml`; restart the agent to apply configuration.

```toml
[notifications]
[[notifications.sinks]]
id = 'owner-discord'
url = 'https://discord.com/api/webhooks/REPLACE_LOCALLY'
format = 'discord' # alternatively 'json' for a receiver of the documented payload
include-message = false
```

Use a real destination only in local protected configuration. The URL may contain
a webhook credential; it is never included in status, Debug output, notifications,
job errors or reports. The example is deliberately unusable. No credential is
required for fixture tests. Configuration is closed: duplicate IDs, unknown fields,
credentials in URL authority, non-HTTPS, non-443 ports, fragments and invalid IDs
refuse. At most 16 sinks are configured. Actual DNS and socket destinations must
be public; private/LAN destinations are unsupported, including when a Vesta server
itself has a trusted LAN connection.

Both formats deliver future inbox entries only. An initial configuration, changed
destination/format/privacy policy, or re-enable does not replay old notifications.
Disabling sinks stops the worker and marks retained delivery state disabled.
Normal reopen with unchanged configuration resumes the recorded cursor/pending
event. Delivery state is per canonical server database, not shared across accounts,
servers or installations. Authenticated `GET /api/v3/manage/notifications/delivery`
requires current `notifications.view` and returns bounded sink IDs, state, cursor,
pending cursor, attempts, next attempt time, delivery/failure counts and retention
gap. It returns no URL, credential, configuration hash or message body.

`json` sends schema1, stable event ID, creation time, severity, title and body.
`discord` sends content with mentions explicitly disabled. By default title/body
are replaced with a generic notice directing the user to Vesta. Enabling
`include-message` explicitly allows the inbox message to leave the machine;
user-authored messages may contain private information. This is not automatic
secret discovery/redaction. Oversize Discord content becomes an explicit
full-details-in-Vesta notice, not a silently truncated message. Generic JSON is
not a native Slack/Teams/email adapter.

Delivery uses the same public-only DNS-pinned, TLS-validating, connected-peer-
checked request implementation as artifact transfers, without a second resolver.
POST has no provider keys/cookies/authorization headers, redirects are never
followed, response bodies are not consumed, and no proxy fallback is attempted.
Notification transport is explicit direct HTTPS; artifact proxy policy is unchanged.
No real external service is contacted unless a sink is configured.

A separate single worker keeps network waits out of automation/status processing.
Each request has a five-second total budget (two-second connect budget), and the
payload has a 64KiB ceiling. Delivery intent is persisted before I/O under one
protected cross-process worker lock. A runtime lease excludes database closure
while delivery is owned; shutdown stops before another sink request and drains the
bounded current request. There is no blanket process kill or accepted-job cancellation.

2xx acknowledges delivery. Redirects and other permanent statuses fail without
following/repeating the request. Transport errors, 5xx and supported 429 retry at
most three attempts per event, spaced at least 30 seconds. Numeric Retry-After up
to one day is respected when longer; unsupported HTTP-date/invalid/longer values
fail closed instead of guessing a shorter delay. Exhaustion advances to subsequent
notifications and increments failures. Inbox retention remains the source queue;
missing retained entries are reported as a retention gap, not guaranteed delivery.

Remote acceptance followed by a lost reply, local persistence failure or a crash
can cause duplicate external delivery on retry. The stable event ID permits a JSON
receiver to deduplicate; Discord does not promise that behavior. Vesta does not
claim exactly-once remote delivery. Original internal notifications remain available
under their existing retention and permission rules.

Automated evidence covers injected sinks, loopback transport mocks and the real
local authenticated TLS status route. Actual Discord delivery, public proxy support
and live third-party behavior require separately configured integration acceptance.
