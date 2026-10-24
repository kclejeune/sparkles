//! The graph overview of a dataset: its classes grouped by community, by property or
//! into their members (spec U06). The computation is in [`crate::overview`].

use crate::Dataset;
use crate::error::Result;
use crate::history::HistoryOptions;
use crate::overview::{FacetsRequest, GroupsRequest, facets_at, groups_at};
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
    ///
    /// A request that groups by GraphRAG reports (`by: {"reports": {}}`) reads the
    /// communities that the dataset's reports graph holds at that state, and is refused
    /// on a branch other than `main`, since the reports belong to main.
    pub fn groups(&self, req: &GroupsRequest, opts: &QueryOptions) -> Result<serde_json::Value> {
        let store = self.ds.store();
        req.check_branch(&store.branch_name())?;
        let opts = self.ds.with_query_defaults(opts);
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

    /// Count the entities of a class, of a list of IRIs or of the values of a query's
    /// variable by ranges or values of their properties (spec U11 §3.2), with each facet
    /// filtered by the brushes on the other facets. The queries run at `req.at` (the head
    /// when `None`) with the dataset's query defaults and the view, budgets, deadline and
    /// cancellation of `opts`. The answer is the JSON object that API.md describes under
    /// "Facets".
    ///
    /// ```
    /// use sparkles::Dataset;
    /// use sparkles::overview::FacetsRequest;
    ///
    /// let ds = Dataset::memory();
    /// ds.update("PREFIX ex: <http://ex/> INSERT DATA { ex:a a ex:T ; ex:age 31 . ex:b a ex:T ; ex:age 44 . ex:c a ex:T }")?;
    /// let req = FacetsRequest::from_json(&serde_json::json!({
    ///     "within": { "class": "http://ex/T" },
    ///     "facets": [{ "property": "http://ex/age" }],
    ///     "filters": [{ "property": "http://ex/age", "range": [30, 40] }],
    /// }))?;
    /// let answer = ds.graph().facets(&req, &Default::default())?;
    /// assert_eq!(answer["size"], 3);
    /// assert_eq!(answer["matching"], 1);
    /// assert_eq!(answer["facets"][0]["missing"], 1);
    /// # Ok::<_, sparkles::Error>(())
    /// ```
    pub fn facets(&self, req: &FacetsRequest, opts: &QueryOptions) -> Result<serde_json::Value> {
        let store = self.ds.store();
        let opts = self.ds.with_query_defaults(opts);
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
        facets_at(snap, req, &opts)
    }
}

impl Dataset {
    /// The graph overview: classes grouped by community, by property or into members.
    pub fn graph(&self) -> Graph {
        Graph { ds: self.clone() }
    }
}
