//! The watch routes of `/$/watches` (spec U12, item 619).

use super::kit::*;
use super::*;

pub(super) fn put_all(put: &mut dyn FnMut(&str, J)) {
    let iri = || with_desc(string(), "An absolute IRI.");
    put(
        "WatchCreate",
        doc(
            obj(
                &["dataset", "entity"],
                json!({
                    "dataset": string(),
                    "branch": with_desc(string(), "The branch, `main` when absent."),
                    "graph": with_desc(string(), "The graph's IRI. Absent, or `default`, names the default graph."),
                    "entity": iri(),
                    "scope": with_desc(string_enum(&["entity", "subtree"]), "`entity` matches commits that name the entity, and `subtree` also those that name a class, property or concept below it in the asserted hierarchy. The default is `entity`."),
                    "depth": with_desc(json!({ "type": "integer", "minimum": 1, "maximum": 1000 }), "For a subtree, the most levels below the entity that count."),
                }),
            ),
            "A new watch.",
            "watches",
        ),
    );
    put(
        "Watch",
        doc(
            obj(
                &[
                    "id",
                    "dataset",
                    "branch",
                    "entity",
                    "scope",
                    "created",
                    "since",
                    "seen",
                    "unread",
                    "overflow",
                    "truncated",
                ],
                json!({
                    "id": string(),
                    "dataset": string(),
                    "branch": with_desc(string(), "The branch's current name. A renamed branch keeps its watches."),
                    "graph": with_desc(string(), "The graph's IRI, absent for the default graph."),
                    "entity": iri(),
                    "scope": string_enum(&["entity", "subtree"]),
                    "depth": int(),
                    "created": string(),
                    "since": with_desc(int(), "The head when the watch was made. Only later commits match."),
                    "seen": with_desc(int(), "The last commit marked as read."),
                    "unread": with_desc(int(), "The matched commits after `seen`."),
                    "latest": with_desc(int(), "The newest matched commit."),
                    "overflow": with_desc(boolean(), "Older matches were dropped, since a watch keeps 200."),
                    "truncated": with_desc(boolean(), "The graph's hierarchy is too large to follow, so the subtree matches only the entity."),
                }),
            ),
            "A watch of the caller's.",
            "watches",
        ),
    );
    put(
        "WatchList",
        doc(
            obj(
                &["watches", "unread"],
                json!({
                    "watches": array(sref("Watch")),
                    "unread": with_desc(int(), "The unread matches of every watch listed."),
                }),
            ),
            "The caller's watches on graphs it can read now.",
            "watches",
        ),
    );
    put(
        "WatchMatch",
        doc(
            obj(
                &["commit", "time", "count", "exact", "unread"],
                json!({
                    "commit": int(),
                    "time": string(),
                    "author": string(),
                    "members": with_desc(strings(), "The watched IRIs the commit's changes named, at most 20 in IRI order."),
                    "count": with_desc(int(), "How many watched IRIs the commit's changes named."),
                    "summary": with_desc(boolean(), "The commit named more IRIs than the `largeCommit` bound, so it is one entry without members."),
                    "partial": with_desc(boolean(), "An ancestor walk stopped at the `maxAncestors` bound, so members may be missing."),
                    "exact": with_desc(boolean(), "`false` when the commit's changes were not listed, such as a bulk load, so it may or may not change the entity."),
                    "own": with_desc(boolean(), "The caller made the commit."),
                    "unread": boolean(),
                }),
            ),
            "A commit a watch matched.",
            "watches",
        ),
    );
    put(
        "WatchMatches",
        doc(
            obj(
                &["watch", "matches", "more"],
                json!({
                    "watch": sref("Watch"),
                    "matches": array(sref("WatchMatch")),
                    "more": boolean(),
                }),
            ),
            "A page of a watch's matched commits, newest first.",
            "watches",
        ),
    );
    put(
        "WatchSeen",
        doc(
            obj(&["commit"], json!({ "commit": int() })),
            "The last commit to mark as read.",
            "watches",
        ),
    );
}
