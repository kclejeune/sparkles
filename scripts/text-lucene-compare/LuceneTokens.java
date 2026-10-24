// Prints the tokens that a Lucene analyzer makes of each line of standard input, one
// line of output per line of input. Tokens are separated by spaces, and a token whose
// position does not follow the last one's is preceded by +N: with its position increment.
// It runs on the classpath of Fuseki's fuseki-server.jar, which bundles Lucene.
//
//   java -cp fuseki-server.jar:. LuceneTokens org.apache.lucene.analysis.fr.FrenchAnalyzer
import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import org.apache.lucene.analysis.Analyzer;
import org.apache.lucene.analysis.TokenStream;
import org.apache.lucene.analysis.tokenattributes.CharTermAttribute;
import org.apache.lucene.analysis.tokenattributes.PositionIncrementAttribute;

public class LuceneTokens {
  public static void main(String[] args) throws Exception {
    Analyzer analyzer = (Analyzer) Class.forName(args[0]).getDeclaredConstructor().newInstance();
    BufferedReader in =
        new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8));
    PrintStream out = new PrintStream(System.out, true, "UTF-8");
    String line;
    while ((line = in.readLine()) != null) {
      StringBuilder sb = new StringBuilder();
      try (TokenStream ts = analyzer.tokenStream("f", line)) {
        CharTermAttribute term = ts.addAttribute(CharTermAttribute.class);
        PositionIncrementAttribute inc = ts.addAttribute(PositionIncrementAttribute.class);
        ts.reset();
        while (ts.incrementToken()) {
          if (sb.length() > 0) sb.append(' ');
          if (inc.getPositionIncrement() != 1) sb.append('+').append(inc.getPositionIncrement()).append(':');
          sb.append(term);
        }
        ts.end();
      }
      out.println(sb);
    }
  }
}
