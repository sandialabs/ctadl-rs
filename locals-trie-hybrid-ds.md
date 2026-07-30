# Locals trie hybrid data structure

Implement a custom hybrid data structure to support a Datalog index on a key `K` with associated
record values `V` -- i.e., like a `Map<K, Set<V>>`. In real workloads, the number of values per `K`
can vary across keys in this map. Some keys have just a couple values (e.g., 2) and some may have
thousands. The map needs to support the Ascent traits but the basic operations are `insert`,
`contains`, `get_all_values_matching_key`, and `a.merge(b)` which means append all values from `b`
into the corresponding keys in `a`.

Let's focus on the `Set<V>` for now. I want a new data structure that behaves like a set but:

- Below a threshold of number of records, behaves like a linear probe hashtable
- Above the threshold, is implemented like a `hashbrown::HashTable`
- The initial threshold should be 32

The data structure should be organized so that transitioning the threshold is efficient.
