// Prints the tokens that Lucene's StandardTokenizer makes of each line of standard
// input, one line of output per line of input: each token as TEXT@START-END:TYPE, with
// the offsets in UTF-16 code units, separated by spaces. It runs on the classpath of
// Fuseki's fuseki-server.jar, which bundles Lucene.
//
//   java -cp fuseki-server.jar:. StandardTokens
import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.io.PrintStream;
import java.io.StringReader;
import java.nio.charset.StandardCharsets;
import org.apache.lucene.analysis.standard.StandardTokenizer;
import org.apache.lucene.analysis.tokenattributes.CharTermAttribute;
import org.apache.lucene.analysis.tokenattributes.OffsetAttribute;
import org.apache.lucene.analysis.tokenattributes.TypeAttribute;

public class StandardTokens {
  public static void main(String[] args) throws Exception {
    BufferedReader in =
        new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8));
    PrintStream out = new PrintStream(System.out, false, "UTF-8");
    StandardTokenizer ts = new StandardTokenizer();
    CharTermAttribute term = ts.addAttribute(CharTermAttribute.class);
    OffsetAttribute off = ts.addAttribute(OffsetAttribute.class);
    TypeAttribute type = ts.addAttribute(TypeAttribute.class);
    String line;
    while ((line = in.readLine()) != null) {
      StringBuilder sb = new StringBuilder();
      ts.setReader(new StringReader(line));
      ts.reset();
      while (ts.incrementToken()) {
        if (sb.length() > 0) sb.append(' ');
        sb.append(term).append('@').append(off.startOffset()).append('-')
            .append(off.endOffset()).append(':').append(type.type());
      }
      ts.end();
      ts.close();
      out.println(sb);
    }
    out.flush();
  }
}
