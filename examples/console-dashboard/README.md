# Console example dashboard

A small dashboard on another origin that reads the `gadk-console`
[JSON API](../../crates/gadk-console/README.md#json-api). It is three static files, `index.html`,
`app.js` (one ES module) and `style.css`, with no build step, no package manager and no
libraries: it uses `fetch`, DOM APIs and an inline SVG chart, and loads nothing but the
console's API. Copy it as a starting point for your own frontend.

## Run it

The JSON API needs the BigQuery trace source. Start the console with this page's origin listed:

```sh
gadk-console --project my-billing-project \
  --traces bigquery://my-data-project.agent_analytics.agent_events \
  --api-origin http://localhost:5173
```

Add `--prices <file>` to see costs; without it every cost shows `—`.

Serve this folder on port 5173:

```sh
cd examples/console-dashboard
python3 -m http.server 5173
```

Open <http://localhost:5173/>. The page reads the console at `http://127.0.0.1:8765` unless the
`console` query parameter names another one, such as
<http://localhost:5173/?console=http://127.0.0.1:9000>. The header's field and range picker
change both; every loaded panel then reloads.

The page's origin must match an `--api-origin` value exactly: open `http://localhost:5173`, not
`http://127.0.0.1:5173`, or list that origin too.

## Panels

| Panel | Route |
|---|---|
| Usage (totals of calls, tokens and cost) | `/api/v1/usage/breakdown?range=<range>` |
| Trend (cost per bucket, or calls without prices) | `/api/v1/usage/trend?range=<range>` |
| Top traces | `/api/v1/usage/top-traces?range=<range>` |
| Sessions, with "More" following `next` | `/api/v1/sessions?range=<r7>` |
| Turns of the clicked session | `/api/v1/sessions/{id}/turns?range=<r7>` |
| Users | `/api/v1/users?range=<r7>` |
| Review queue, loaded with its button | `/api/v1/review/pending?score=<name>&range=<r7>` |

`<r7>` is the picked range, except that `30d` is sent as `7d`: the sessions, users and review
routes cover at most 7 days, and the page says so while `30d` is picked. Trace ids link to the
console's trace page.

Each panel loads on its own. A failed request shows a banner in that panel only:

- the API's error message and hint;
- "The console needs --traces bigquery://… for the JSON API." without the BigQuery source;
- "Cannot reach <console>: is it running with --api-origin <origin>?" when `fetch` fails, which
  is what a stopped console and a missing `--api-origin` both look like to the page.

## Safety

`app.js` renders API data only through `textContent`, `createElement` and attribute setters,
never through HTML strings, and encodes every id it puts in a path with `encodeURIComponent`.
`fetch` keeps its default mode, so cross-origin requests carry no cookies. A unit test in
`gadk-console` (`src/web/api/dashboard_tests.rs`) checks that every `/api/v1/` path in `app.js`
is a console route and that the file has none of the HTML-string sinks.
