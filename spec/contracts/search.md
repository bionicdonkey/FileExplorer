# Search

## Scope

A search request names a **scope**: a root location, a maximum depth (the root is depth 0, so 1
includes direct children), a maximum number of results and a time limit. The default for a local
search is depth 1, 5000 results and 2 seconds. The scope that the provider reports back, the
**declared scope**, must equal the one requested; an answer for any other scope is not trusted and
is shown as an incomplete search with reason `malformed_page`.

## Outcomes

| Outcome | Meaning |
| --- | --- |
| `completed` | The search ran to the end of its scope. Carries the matches, whether the scope was limited, and the **limits** in force. |
| `incomplete` | It stopped early. Carries the matches found so far, a **reason** and a retry class. |
| `unsupported` | This provider cannot do this search at all (for example content search with no index, or a remote root). It is an explicit answer, never an empty result. |

Reasons for `incomplete`: `cancelled`, `provider_failure`, `malformed_page`, `access_lost`.

`limits` lists the bounds that were in force and that may have shortened the result: `depth`,
`result_count`, `time`. A provider must list a limit that actually cut the search short, and may
list the others, so a scenario checks inclusion. Matches in a completed or incomplete outcome are
de-duplicated by location; an incomplete search keeps its matches and can be retried with the same
query and scope. [`adapter/search/depth-order-result-and-time-bounds`,
`adapter/search/cancellation-failure-and-unavailable-modes-are-explicit`,
`domain/search/incomplete-keeps-results-and-rejects-a-navigation-race`]

## Matching and order

* Filename search matches the name. Content search needs an index; without one it is `unsupported`.
* Results come back in name order when the provider's order is stable.
* A search never follows a symbolic link, and a location is never recovered by decoding a name as a
  URI: a file called `report-% space` is that exact path.
  [`adapter/search/never-follows-symlinks-or-decodes-names-as-uris`]

## What the application does around it

* Typing is **debounced** by 180 ms: each keystroke supersedes the previous query immediately, and
  only the last one reaches the provider. Closing the tab or clearing cancels the pending query, and
  a late answer is dropped. [`domain/search/debounce-sends-only-the-latest-query-and-close-cancels`]
* A newer query cancels an older one; the older query's answer is ignored even if it arrives later.
  [`domain/search/newest-query-wins-and-clear-restores-the-saved-selection`]
* A search is refused while the tab has no loaded directory or a navigation is pending, so it never
  binds to a stale or unfinished location, and typing never cancels a directory load.
  [`domain/search/typing-while-loading-neither-cancels-navigation-nor-binds-to-the-old-root`]
* Search is a temporary overlay on the tab. It does not touch navigation history. Clearing it
  restores the directory and the selection that were there. An answer that arrives after a new
  navigation began is ignored.
* **Privacy.** Search terms are saved only when the preference allows it. Submitting with the
  preference off removes every saved term, in memory and on disk, and saving respects the retention
  period. [`domain/search/history-honours-the-privacy-policy`]

## Platform differences

Content search depends on the platform's index (Tracker on Linux, Spotlight on macOS); its coverage
and ranking differ, so content search is `approximate` on macOS. Filename search is held to the same
scenarios on both.
