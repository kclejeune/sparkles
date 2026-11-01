//! The one execution policy. Every entry point, the HTTP server, the CLI, the MCP server,
//! GraphQL and the bindings, describes what its consumer of an answer needs as a
//! [`Consumer`], and [`choose_execution`] decides from that how the query runs.

use super::cursor::ExecutionMode;

/// What the consumer of a query's answer needs from its execution.
///
/// A caller fills in the properties it knows and leaves the rest at their defaults. The
/// requested mode is what the caller asked for, and `default` is the mode of a caller that
/// asked for none. The other properties are needs. Some of them only steer the default,
/// and others rule out a cursor whatever was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Consumer {
    /// The mode the caller asked for, when it asked for one.
    pub requested: Option<ExecutionMode>,
    /// The mode that runs when the caller asked for none.
    pub default: ExecutionMode,
    /// The consumer must see every row before it writes the first, as a text table that
    /// measures its columns does. Without a requested mode the query runs eagerly.
    pub exact_total: bool,
    /// The consumer prints the eager plan explanation. Without a requested mode the query
    /// runs eagerly.
    pub plan: bool,
    /// The consumer reads the whole answer at once, as the MCP tools, GraphQL and the eager
    /// query APIs do. Without a requested mode the query runs eagerly.
    pub whole_result: bool,
    /// The query runs inside a write transaction, whose uncommitted state no cursor
    /// reads. A query that asked for no mode runs eagerly, and one that asked for a
    /// cursor or for automatic execution is refused.
    pub in_transaction: bool,
    /// The answer's format has a cursor writer. SPARQL Results Thrift does not, so
    /// automatic execution runs eagerly for it.
    pub cursor_format: bool,
    /// The query has initial bindings, which rule out every automatic plan, so automatic
    /// execution runs eagerly.
    pub initial_bindings: bool,
}

impl Default for Consumer {
    fn default() -> Self {
        Consumer {
            requested: None,
            default: ExecutionMode::Auto,
            exact_total: false,
            plan: false,
            whole_result: false,
            in_transaction: false,
            cursor_format: true,
            initial_bindings: false,
        }
    }
}

impl Consumer {
    /// A consumer that asked for `mode`, or for none.
    pub fn requesting(mode: Option<ExecutionMode>) -> Consumer {
        Consumer {
            requested: mode,
            ..Consumer::default()
        }
    }

    /// A consumer that reads the whole answer at once and asked for no mode.
    pub fn whole_result() -> Consumer {
        Consumer {
            whole_result: true,
            ..Consumer::default()
        }
    }
}

/// Why a consumer's request cannot run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyError {
    /// A cursor or automatic execution was asked for inside a write transaction.
    CursorInTransaction,
}

impl PolicyError {
    /// The message the bindings give for the refusal.
    pub fn message(self) -> &'static str {
        match self {
            PolicyError::CursorInTransaction => {
                "finish the transaction before opening a streaming cursor"
            }
        }
    }
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for PolicyError {}

impl From<PolicyError> for crate::error::Error {
    fn from(e: PolicyError) -> Self {
        crate::error::Error::invalid(e.message())
    }
}

impl ExecutionMode {
    /// The mode a request names as `eager`, `streaming` or `auto`.
    pub fn parse(name: &str) -> Option<ExecutionMode> {
        match name {
            "eager" => Some(ExecutionMode::Eager),
            "streaming" => Some(ExecutionMode::Streaming),
            "auto" => Some(ExecutionMode::Auto),
            _ => None,
        }
    }

    /// The name [`ExecutionMode::parse`] reads.
    pub fn name(self) -> &'static str {
        match self {
            ExecutionMode::Eager => "eager",
            ExecutionMode::Streaming => "streaming",
            ExecutionMode::Auto => "auto",
        }
    }
}

/// How a query for `consumer` runs.
///
/// A requested mode wins over the default and over the needs that only steer the
/// default. Inside a write transaction a requested cursor or automatic execution is
/// refused. Automatic execution then runs eagerly when the answer's format has no cursor
/// writer or the query has initial bindings, since no automatic plan would stream.
pub fn choose_execution(consumer: &Consumer) -> Result<ExecutionMode, PolicyError> {
    let mode = match consumer.requested {
        Some(ExecutionMode::Eager) => return Ok(ExecutionMode::Eager),
        Some(_) if consumer.in_transaction => return Err(PolicyError::CursorInTransaction),
        Some(mode) => mode,
        None if consumer.in_transaction
            || consumer.exact_total
            || consumer.plan
            || consumer.whole_result =>
        {
            ExecutionMode::Eager
        }
        None => consumer.default,
    };
    if mode == ExecutionMode::Auto && (!consumer.cursor_format || consumer.initial_bindings) {
        return Ok(ExecutionMode::Eager);
    }
    Ok(mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ExecutionMode::{Auto, Eager, Streaming};

    const MODES: [Option<ExecutionMode>; 4] = [None, Some(Eager), Some(Streaming), Some(Auto)];

    /// The HTTP rules before the policy moved into core: the parameter, or the server's
    /// default, and automatic execution kept eager for Thrift and for initial bindings.
    fn http_before(
        requested: Option<ExecutionMode>,
        default: ExecutionMode,
        thrift: bool,
        bindings: bool,
    ) -> ExecutionMode {
        let execution = requested.unwrap_or(default);
        if execution == Streaming {
            return Streaming;
        }
        let auto = execution == Auto && !thrift && !bindings;
        if auto { Auto } else { Eager }
    }

    #[test]
    fn http_chooses_as_before() {
        for requested in MODES {
            for default in [Eager, Auto] {
                for thrift in [false, true] {
                    for bindings in [false, true] {
                        let consumer = Consumer {
                            requested,
                            default,
                            cursor_format: !thrift,
                            initial_bindings: bindings,
                            ..Consumer::default()
                        };
                        assert_eq!(
                            choose_execution(&consumer),
                            Ok(http_before(requested, default, thrift, bindings)),
                            "{consumer:?}"
                        );
                    }
                }
            }
        }
    }

    /// The CLI rules before: `--execution`, or eager for the text table and for
    /// `--explain`, and automatic execution otherwise.
    fn cli_before(requested: Option<ExecutionMode>, text: bool, explain: bool) -> ExecutionMode {
        match requested {
            Some(mode) => mode,
            None if text || explain => Eager,
            None => Auto,
        }
    }

    #[test]
    fn the_cli_chooses_as_before() {
        for requested in MODES {
            for text in [false, true] {
                for explain in [false, true] {
                    let consumer = Consumer {
                        requested,
                        exact_total: text,
                        plan: explain,
                        ..Consumer::default()
                    };
                    assert_eq!(
                        choose_execution(&consumer),
                        Ok(cli_before(requested, text, explain)),
                        "{consumer:?}"
                    );
                }
            }
        }
    }

    /// The bindings before: automatic execution by default, eager inside a write
    /// transaction, and a refusal of a cursor or of automatic execution asked for there.
    #[test]
    fn the_bindings_choose_as_before() {
        for requested in MODES {
            let outside = choose_execution(&Consumer::requesting(requested));
            assert_eq!(outside, Ok(requested.unwrap_or(Auto)));
            let inside = choose_execution(&Consumer {
                in_transaction: true,
                ..Consumer::requesting(requested)
            });
            let expected = match requested {
                None | Some(Eager) => Ok(Eager),
                Some(_) => Err(PolicyError::CursorInTransaction),
            };
            assert_eq!(inside, expected, "{requested:?}");
        }
    }

    #[test]
    fn whole_result_consumers_run_eagerly() {
        assert_eq!(choose_execution(&Consumer::whole_result()), Ok(Eager));
    }

    #[test]
    fn modes_parse_their_names() {
        for mode in [Eager, Streaming, Auto] {
            assert_eq!(ExecutionMode::parse(mode.name()), Some(mode));
        }
        assert_eq!(ExecutionMode::parse("lazy"), None);
    }
}
