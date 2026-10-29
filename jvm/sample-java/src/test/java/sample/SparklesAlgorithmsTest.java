package sample;

import static org.junit.jupiter.api.Assertions.*;
import io.github.kclejeune.sparkles.jena.SparklesDatasets;
import org.apache.jena.atlas.json.JSON;
import org.junit.jupiter.api.Test;

class SparklesAlgorithmsTest {
    @Test
    void javaCanUseAlgorithmHandlesAndDefaults() {
        try (var ds = SparklesDatasets.memory()) {
            var algorithms = ds.algorithms();
            var projections = algorithms.projections();
            assertTrue(projections.put("p", JSON.parse("{\"predicates\":[\"urn:p\"]}")).getBoolean("created"));
            assertNotNull(projections.get("p"));
            assertTrue(algorithms.queryText(JSON.parse("{\"algorithm\":\"wcc\",\"projection\":\"p\"}")).contains("SELECT"));
            assertTrue(projections.delete("p"));
        }
    }
}
