"""A fluent SPARQL query builder, the bindings of `sparkles::querybuilder` (Jena's
query builder in Rust).

>>> from sparkles import Dataset, Literal
>>> from sparkles.querybuilder import SelectBuilder, WhereBuilder
>>> q = (
...     SelectBuilder()
...     .prefix("ex", "http://ex.org/")
...     .select("?name")
...     .where_("?p", "ex:name", "?name")
...     .optional(WhereBuilder().where_("?p", "ex:age", "?age"))
...     .filter("!BOUND(?age) || ?age > 30")
...     .order_by("?name")
... )
>>> print(q.build())
PREFIX ex: <http://ex.org/>
SELECT ?name
WHERE {
  ?p ex:name ?name .
  OPTIONAL {
    ?p ex:age ?age .
  }
  FILTER(!BOUND(?age) || ?age > 30)
}
ORDER BY ?name

Every method returns a new builder, so a partly built query serves as a template. A
`str` argument is SPARQL term syntax (`"?x"`, `"<http://ex.org/a>"`, `"ex:name"`, `"a"`,
or a property path in predicate position). Values go in as terms, numbers or booleans,
which are always escaped: `set_var("?name", Literal(user_input))` makes a prepared
query. Expressions are SPARQL expression text. `build()` checks the text with the
SPARQL parser, and `str(builder)` gives it unchecked.
"""

from __future__ import annotations

from sparkles._sparkles import (
    AskBuilder,
    ConstructBuilder,
    DescribeBuilder,
    SelectBuilder,
    UpdateBuilder,
    WhereBuilder,
)

__all__ = [
    "AskBuilder",
    "ConstructBuilder",
    "DescribeBuilder",
    "SelectBuilder",
    "UpdateBuilder",
    "WhereBuilder",
]
