public final class MultiImplFlow {
    interface Processor { String process(String s); }
    static class TaintProcessor implements Processor {
        public String process(String s) { return s; } // Line 4
    }
    static class SafeProcessor implements Processor {
        public String process(String s) { return "safe"; }
    }

    static String source() { return "tainted"; }
    static void sink(String s) { System.out.println(s); }

    public static void main(String[] args) {
        String data = source(); // Line 14
        Processor p1 = new TaintProcessor();
        Processor p2 = new SafeProcessor();
        
        sink(p1.process(data)); // Line 18
        sink(p2.process(data)); // Line 19
    }
}
