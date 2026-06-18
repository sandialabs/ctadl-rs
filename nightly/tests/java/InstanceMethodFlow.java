public final class InstanceMethodFlow {
    static String source() { return "tainted"; }
    static void sink(String s) { System.out.println(s); }

    static class DataHolder {
        String data;
        void setData(String d) { this.data = d; } // Line 7
        String getData() { return this.data; }    // Line 8
    }

    public static void main(String[] args) {
        DataHolder holder = new DataHolder();
        holder.setData(source()); // Line 13
        String retrieved = holder.getData(); // Line 14
        sink(retrieved); // Line 15
    }
}
