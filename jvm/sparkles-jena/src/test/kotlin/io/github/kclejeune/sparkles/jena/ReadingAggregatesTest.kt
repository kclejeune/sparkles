package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.engine.readsErrorsIn
import org.apache.jena.query.QueryFactory
import org.apache.jena.query.Syntax
import org.apache.jena.sparql.algebra.Algebra
import org.apache.jena.sparql.algebra.op.OpGroup
import org.apache.jena.sparql.algebra.op.OpTable
import org.apache.jena.sparql.core.Var
import org.apache.jena.sparql.core.VarExprList
import org.apache.jena.sparql.expr.E_Exists
import org.apache.jena.sparql.expr.ExprAggregator
import org.apache.jena.sparql.expr.aggregate.AggregatorFactory
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** The search for aggregates whose answer depends on the reading of errors, which decides whether ARQ may answer. */
class ReadingAggregatesTest {
    private fun compiled(q: String) = Algebra.compile(QueryFactory.create(q, Syntax.syntaxARQ))
    private val inner = "{ { SELECT (MIN(?v) AS ?m) { VALUES ?v { 1 UNDEF } } } FILTER(BOUND(?m)) }"

    @Test fun every_expression_position_is_searched() {
        for (q in listOf(
            "SELECT (MIN(?v) AS ?m) { VALUES ?v { 1 } }",
            "SELECT (EXISTS $inner AS ?ok) {}",
            "SELECT ?x { VALUES ?x { 1 } FILTER NOT EXISTS $inner }",
            "SELECT ?x { VALUES ?x { 1 2 } } ORDER BY (IF(?x = 1, EXISTS $inner, false)) ?x",
            "SELECT ?k (COUNT(*) AS ?n) { VALUES ?x { 1 } } GROUP BY (EXISTS $inner AS ?k)",
            "SELECT (COUNT(*) AS ?n) { VALUES ?x { 1 } } HAVING (EXISTS $inner)",
            "SELECT ?ok { BIND(EXISTS $inner AS ?ok) }",
            "SELECT ?x ?y { VALUES ?x { 1 } OPTIONAL { VALUES ?y { 2 } FILTER(EXISTS $inner) } }",
            "SELECT ?x { VALUES ?x { 1 } FILTER EXISTS { FILTER EXISTS $inner } }",
            "PREFIX cdt: <http://w3id.org/awslabs/neptune/SPARQL-CDTs/> SELECT ?x { " +
                "UNFOLD(IF(EXISTS $inner, \"[1]\"^^cdt:List, \"[2]\"^^cdt:List) AS ?x) }",
        )) assertTrue(readsErrorsIn(compiled(q)), q)
    }

    @Test fun an_aggregate_argument_is_searched() {
        // SPARQL refuses an aggregate inside another's argument, even within EXISTS, so
        // the algebra is built directly
        val exists = E_Exists(compiled("SELECT (MIN(?v) AS ?m) { VALUES ?v { 1 } }"))
        val sum = ExprAggregator(Var.alloc("n"), AggregatorFactory.createSum(false, exists))
        assertTrue(readsErrorsIn(OpGroup.create(OpTable.unit(), VarExprList(), listOf(sum))))
    }

    @Test fun other_aggregates_are_not_found() {
        val count = "{ { SELECT (COUNT(?v) AS ?m) { VALUES ?v { 1 UNDEF } } } FILTER(BOUND(?m)) }"
        for (q in listOf(
            "SELECT (SUM(?v) AS ?s) (COUNT(?v) AS ?n) (SAMPLE(?v) AS ?a) { VALUES ?v { 1 } }",
            "SELECT ?x { VALUES ?x { 1 2 } } ORDER BY (EXISTS $count) ?x",
        )) assertFalse(readsErrorsIn(compiled(q)), q)
    }
}
