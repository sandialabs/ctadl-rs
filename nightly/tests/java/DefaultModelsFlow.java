/* Exercises the SHIPPED Java propagation defaults
 * (`ctadl-ascent/src/models/defaults/java-index.jsonl`).
 *
 * This case supplies no propagation model of its own -- only a source on
 * source() and a sink on sink(). Every step between them is a JDK method with
 * no body in this artifact, so each of the three flows exists only if the
 * default file is loaded for a Java import and models it. All three sink lines
 * are required, so one working flow cannot cover for two broken ones.
 *
 *   StringBuilder.append   Argument(1) -> Argument(0)
 *   List.add / List.get    Argument(1) -> Argument(0).\[]  /  the reverse
 *   Map.put / Map.get      the same pair, on the java.util generators
 *
 * The container halves are the ones worth having a test for. They are modeled
 * at the ELEMENT level, so a write and a read compose only if both name the
 * array element the frontends actually emit -- `Symbol("[]")`, written `.\[]`.
 * They used to name a synthetic `.rep` that no frontend writes, which composed
 * add->get with itself and joined nothing else; and a bare read against an
 * element-level write composes to nothing at all. Neither mistake fails
 * loudly.
 */
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

public class DefaultModelsFlow {
  static String source() {
    return "tainted";
  }

  static void sink(String x) {
    System.out.println(x);
  }

  public static void main(String[] args) {
    StringBuilder sb = new StringBuilder();
    sb.append(source());
    sink(sb.toString());

    List<String> list = new ArrayList<String>();
    list.add(source());
    sink(list.get(0));

    Map<String, String> map = new HashMap<String, String>();
    map.put("k", source());
    sink(map.get("k"));
  }
}
