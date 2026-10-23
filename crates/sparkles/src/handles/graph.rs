//! The graph overview of a dataset: its classes grouped by community, by property or
//! into their members (spec U06). The computation is in [`crate::overview`].

use crate::Dataset;
use crate::error::Result;
use crate::history::HistoryOptions;
use crate::overview::{GroupsRequest, groups_at};
use crate::sparql::QueryOptions;

/// The dataset's graph overview (from [`Dataset::graph`]).
#[derive(Clone)]
pub struct Graph {
    pub(crate) ds: Dataset,
}

impl Graph {
    /// Group the instances of a class, or of a group an earlier answer named, and count
    /// the edges between the groups, to their sibling groups and to the other classes.
    /// The queries run at `req.at` (the head when `None`) with the dataset's query
    /// defaults and the view, budgets, deadline and cancellation of `opts`. The answer is
    /// the JSON object that API.md describes under "Graph overview".
    ///
    /// ```
    /// use sparkles::Dataset;
    /// use sparkles::overview::GroupsRequest;
    ///
    /// let ds = Dataset::memory();
    /// ds.update("PREFIX ex: <http://ex/> INSERT DATA { ex:a a ex:T ; ex:k ex:b . ex:b a ex:T }")?;
    /// let req = GroupsRequest::from_json(&serde_json::json!({ "within": { "class": "http://ex/T" } }))?;
    /// let answer = ds.graph().groups(&req, &Default::default())?;
    /// assert_eq!(answer["within"]["size"], 2);
    /// assert_eq!(answer["members"].as_array().unwrap().len(), 2);
    /// # Ok::<_, sparkles::Error>(())
    /// ```
    pub fn groups(&self, req: &GroupsRequest, opts: &QueryOptions) -> Result<serde_json::Value> {
        let opts = self.ds.with_query_defaults(opts);
        let store = self.ds.store();
        let snap = match &req.at {
            None => store.snapshot(),
            Some(at) => {
                let o = HistoryOptions {
                    cancel: opts.cancel.clone(),
                    deadline: opts.deadline,
                };
                store.snapshot_at(at, &o)?.0
            }
        };
        groups_at(snap, req, &opts)
    }
}

impl Dataset {
    /// The graph overview: classes grouped by community, by property or into members.
    pub fn graph(&self) -> Graph {
        Graph { ds: self.clone() }
    }
}
