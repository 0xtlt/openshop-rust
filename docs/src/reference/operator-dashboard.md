# Operator dashboard

The operator dashboard shows what happens in every shop: the installations,
the runs, their steps and their logs. It is served under `/ops`, outside
Shopify admin, behind a password. It is for whoever runs the application, not
for the merchants: a merchant never reaches it.

It is off by default. Without a password, `/ops` answers `404`.

## Turn it on

The `ops_password` setting of `app!` takes the password, as an
`Option<String>`. The application reads it from wherever it keeps its secrets:
OpenShop does not read a variable of its own.

```rust,ignore
use openshop::prelude::*;

static APP: App = app! {
    flows: [sync_orders],
    ops_password: std::env::var("OPS_PASSWORD").ok(),
};
```

`OPS_PASSWORD` is the name the `openshop init` template uses; it is a variable
of the application, read by its own code.

| `ops_password` | `/ops` |
| --- | --- |
| Absent, `None`, empty, or only whitespace | `404`, as for any path that does not exist. |
| A password | The dashboard, behind the sign-in form. |
| A password shorter than 16 characters, with `OPENSHOP_ENV=production` | The application does not start: `[openshop] Invalid config: opsPassword must be at least 16 characters in production`. |

`ops` is a reserved first segment: a [custom admin page](custom-admin-pages.md)
cannot be declared at `/ops`.

## Pages

A menu on the side leads to six pages. `Overview`, `Runs`, `Logs`, `Shops`,
and `Flows` look at a period: the last hour, the last 24 hours (the default),
the last 7 days, or the last 30 days, chosen at the top of the page and kept
in the address as `range=1h`, `24h`, `7d`, or `30d`.

| Path | Shows |
| --- | --- |
| `/ops` | The period at a glance: the installed shops, the runs and the share that failed, the runs that wait, with how long the oldest has waited, and those a worker has. Then a chart of the runs and the failures over the period, the flows and the shops with the most failures, and the 10 latest failures. Reloads every 30 seconds. |
| `/ops/runs` | The runs of every shop over the period, the latest first, under a chart of them. Filters: `status`, `flow`, a part of the `shop` domain, and `q`, a part of the error. |
| `/ops/runs/:id` | A run: its shop, status, attempts, timestamps, duration, error and input, then its step attempts with their output, and its first 500 log lines with their payload. |
| `/ops/logs` | The lines the flows of every shop wrote over the period, the latest first, under a chart of them and of the errors among them. Filters: `level`, `flow`, a part of the `shop` domain, and `q`, a part of the message. A line leads to its run. |
| `/ops/shops` | The installations, `installed`, `uninstalled`, or `pending` when the shop started the installation without finishing it, with the runs and the failures of the period. The shops with the most failures come first. Filters: `state`, and `q`, a part of the domain. |
| `/ops/shops/:shop` | A shop: its runs over the period, its installation and scopes, whether it saved each provider and how the last check went, the crons it turned off, and its 10 latest runs. |
| `/ops/flows` | The flows of the application, and those that ran without being registered any more: runs, failures, failure rate, mean duration, and latest run over the period. A flow leads to its runs. |
| `/ops/crons` | The crons of the application: flow, schedule, the shops they run for, the next run, and how many shops turned each off. |
| `/ops/login` | The sign-in form. |

Lists show 100 rows; `Older` at their end leads to the next ones, with
`before`, an RFC 3339 timestamp, in the address. A filter in force is shown
as a chip that removes it. A row of a table is a link wherever it is clicked:
to the run, the shop, or the runs of the flow it is about.

A search matches a part of a text, whatever its case, and nothing in it is a
pattern. Timestamps are in UTC. Rows name their Shopify app when the
application serves [several](../guides/configure-shopify-apps.md).

The dashboard only reads. It has no way to retry or cancel a run, change a
provider, or toggle a cron, and every method other than `GET` on its pages
answers `404`. The access token of a shop and the configuration of a provider
are never read: of a provider, a page says whether the shop saved it and how
its last check went.

The pages are HTML rendered by the server, without script, App Bridge, or the
WebAssembly bundle of the admin, and load nothing from another host: charts
are drawn by the server as SVG. They work in any browser, with or without an
[admin UI](custom-admin-pages.md#serve-the-bundle), follow its light or dark
theme, and on a narrow screen show each row of a table as a card.

Counting over a period reads the runs and the log lines of that period. On a
large history, prefer a short period: the longest ones take the time it takes
to read them. The log lines are read through the indexes of the
[framework schema](database.md#framework-tables) on their time and their
level, and a statement of the dashboard is cancelled after 10 seconds: a page
that cannot be computed in that time answers with an error and does not hold
the database. Set [`log_retention`](configuration.md#settings) to keep the
table, and so the pages, small.

## Session

| Request | Answer |
| --- | --- |
| Any path under `/ops` without a session | `303` to `/ops/login`. |
| `POST /ops/login` with the password, as the form field `password` | `303` to `/ops`, with the session cookie. |
| `POST /ops/login` with another password | `401` and the form, after a delay. |
| `POST /ops/login` while another attempt is being checked | `429` and the form. The password was not looked at. |
| `POST /ops/login` or `POST /ops/logout` sent by a page of another site (`Sec-Fetch-Site`) | `403`. |
| `POST /ops/logout` | `303` to `/ops/login`; the browser forgets the cookie. |

The session is the cookie `openshop_ops`: `HttpOnly`, `SameSite=Strict`,
`Path=/ops`, 12 hours, and `Secure`, except in `dev` and when `HOST` is a
plain `http://` URL, where a browser would not keep it. It is signed and holds its
expiry: no table stores it, and every web process of a deployment accepts it.
The signing key is derived from the password, and from `ENCRYPTION_KEY` when
it is set, so that changing either closes every session. Signing out removes
the cookie from one browser; the session itself stays valid until it expires.

Passwords are compared in constant time. One attempt is checked at a time, and
a wrong one is answered after a delay that doubles with each failure, from
250 ms to 8 seconds, until a successful sign-in. Guessing stays slow whatever
the number of connections. While someone keeps guessing, signing in is refused
for everyone; a session that is already open keeps working.

Each sign-in and each wrong password writes a line to the
[runtime logger](logging.md), without the password.

## Response headers

Every response under `/ops` carries:

| Header | Value |
| --- | --- |
| `Cache-Control` | `no-store` |
| `Content-Security-Policy` | `default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'` |
| `X-Frame-Options` | `DENY` |
| `Referrer-Policy` | `no-referrer` |
| `X-Content-Type-Options` | `nosniff` |
| `X-Robots-Tag` | `noindex, nofollow, noarchive, nosnippet`, as on every response |

Everything a page shows that comes from a shop, a flow, or a log is written as
text, never as markup, and no page runs a script.

## What a shared password does not give

The password is one secret for everyone who operates the application. Nothing
records who signed in, and the logs a run wrote may hold data of the
merchants. Serve the application over HTTPS, give the password to the people
who need it, and change it when one of them leaves. Leave `ops_password` out
to do without the dashboard: [the records](database.md#read-the-framework-tables)
it reads are available to the code of the application, behind whatever
authentication [a route](server-routes.md#authentication) of yours declares.

## With an admin closed to merchants

The dashboard does not depend on what the merchants see. An application whose
merchants only reach its own pages disables the built-in ones, and keeps the
runs and the logs for its operator:

```rust,ignore
use openshop::AdminPagesConfig;
use openshop::prelude::*;

static APP: App = app! {
    flows: [sync_orders],
    admin_pages: [HomePage, SettingsPage],
    builtin_pages: AdminPagesConfig::none(),
    ops_password: std::env::var("OPS_PASSWORD").ok(),
};
```

See [Admin pages](configuration.md#admin-pages) for `AdminPagesConfig::none()`
and [the home of the admin](custom-admin-pages.md#the-home-of-the-admin) for
`HomePage`.
