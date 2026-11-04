# Reading your synced data (portfolio / dashboards)

The cloud database is a normal Turso/libSQL database that you own. Everything
below works with a **read-only token** — create one in the Turso dashboard
(database → *Connect* / *Tokens*) and share that, never the write token the
daemon uses.

> The daemon uploads only app names, categories, and time intervals — no
> titles, paths, or keystrokes. See [../PRIVACY.md](../PRIVACY.md).

## Schema (created automatically by the first sync)

| table | grain | notes |
|---|---|---|
| `segments` | one row per usage segment | immutable; pruned after 90 days |
| `batches` | one row per upload | replay protection |
| `devices` | one row per machine | random UUID + first/last seen |
| `daily_app` | one row per day × app | pre-aggregated, recomputed on write |
| `daily_unique` | one row per day | union stats, recomputed on write |

`daily_app`:

| column | meaning |
|---|---|
| `combined_seconds` | plain Σ of all segments for that app that day (can exceed 24 h across devices) |
| `merged_seconds` | union of that app's intervals — overlapping time counted once |
| `updated_at` | unix seconds of the last recompute |

`daily_unique`:

| column | meaning |
|---|---|
| `unique_active_seconds` | union of **all** apps/devices that day (≤ 86400) |
| `overlap_seconds` | Σ per-app merged − global union: time two apps were active at once |

Two numbers are deliberately different: `combined_seconds` is "how much app
usage happened" (a sum you can rank apps by), `merged_seconds` is "how much
wall-clock time this app was in front". Multi-device overlap shows up once in
`overlap_seconds`, never inside `unique_active_seconds`.

## Queries

Daily active time, last 30 days:

```sql
SELECT activity_date,
       unique_active_seconds / 3600.0 AS hours,
       overlap_seconds      / 3600.0 AS overlap_hours
FROM daily_unique
ORDER BY activity_date DESC
LIMIT 30;
```

Top apps, last 30 days (this is the portfolio-friendly one — pre-aggregated,
no raw scan):

```sql
SELECT app_name,
       SUM(merged_seconds)   / 3600.0 AS hours,
       SUM(combined_seconds) / 3600.0 AS usage_hours
FROM daily_app
WHERE activity_date >= date('now', '-30 days')
GROUP BY app_name
ORDER BY hours DESC;
```

Per-device contribution (needs the raw table, within its 90-day window):

```sql
SELECT device_id,
       COUNT(*)                     AS segments,
       SUM(duration_seconds)/3600.0 AS hours
FROM segments
WHERE activity_date >= date('now', '-30 days')
GROUP BY device_id;
```

Per-app totals cannot be split by device from the aggregates (they are
cross-device by design); use the raw table when you need that attribution.

Calendar heatmap (last 90 days of aggregates — aggregates are kept after raw
segments are pruned):

```sql
SELECT activity_date, unique_active_seconds
FROM daily_unique
WHERE activity_date >= date('now', '-90 days')
ORDER BY activity_date;
```

## Cost / performance notes

- All dashboard queries above touch only pre-aggregated tables (`daily_app`,
  `daily_unique`) except the per-device one. They stay fast regardless of how
  many raw segments exist.
- Rows are one per app per day (plus one per day), so a year of data is a few
  thousand rows — cheap enough for a free-tier database.
- Do not point a dashboard at the write token. Read-only tokens cannot modify
  the tables the daemon recomputes.

## Verifying the connection

On a machine with the config in place (URL + token):

```bash
atw sync --check
# connected — cloud has 1234 stored segment(s)
```

This creates the schema if needed and counts segments; it uploads nothing.

## Deleting an application's cloud history

`atw delete-app "Display Name"` (also wired to the panel's hover-delete) runs
one transaction against the same tables this document queries:

1. `SELECT DISTINCT activity_date` of the app's segments, `SELECT COUNT(*)`
   as the reported deletion count.
2. `DELETE FROM segments WHERE app_name = ?` — gone everywhere, for every
   device that uploaded it.
3. `DELETE FROM daily_app WHERE app_name = ?` — the per-app aggregate rows.
4. For each affected day: `DELETE FROM daily_unique WHERE activity_date = ?`
   and one `RECOMPUTE_DAILY_UNIQUE` pass — the global union is rebuilt from
   the *remaining* segments, so dashboards never show stale seconds that
   belonged to the deleted app.

Because the transaction also held the segment deletes, a failed purge can
never leave `daily_*` rows pointing at deleted segments. Dashboards that
were mid-query may briefly see slightly smaller day totals; the next
recompute settles them.

The purge also appends the app's window classes to
`tracking.excluded_apps` on the machine it was run on — the cloud has no
concept of "excluded", devices simply stop uploading that app. Other
machines' *local* copies of the purged segments age out through their own
retention; the ingest-log cursor used by pulls never revisits them, since
their `ingested_at` is behind every active cursor.
