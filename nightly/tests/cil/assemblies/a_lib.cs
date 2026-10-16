// The library of the multi-assembly case.
namespace Lib
{
    public static class Util
    {
        public static string Pass(string s) => s;
    }

    public class Widget
    {
        public virtual string Render(string s) => "clean";
    }
}
