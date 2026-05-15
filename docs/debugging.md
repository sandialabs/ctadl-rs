```
cd ~/.local/state/ctadl/projects/backflash/index && duckdb
```

See summaries from the index:

```sql
select * from summary.parquet join function_id.parquet on summary.function_id = function_id.function_id limit 10;
```

Get functions endpoints are in:

```sql
SELECT DISTINCT f.name AS endpoint_function, t.endpoint_label
FROM read_parquet('taint.parquet')      AS t
JOIN read_parquet('function_id.parquet') AS f
ON t.endpoint_infunc = f.id;
```

# Pcode

## Duckdb

Print high PCode in Duckdb:

```sql
SELECT
    bbf."column1",
    printf('%x', target."column1") AS addr,
    o."column1" AS output,
    mnem."column1",
    i0."column2" AS in0,
    i1."column2" AS in1,
    i2."column2" AS in2
FROM read_csv("PCODE_INDEX.facts", header=false) idx
JOIN read_csv("PCODE_MNEMONIC.facts", header=false) mnem USING ("column0") --id
JOIN read_csv("PCODE_TARGET.facts", header=false) target USING ("column0") --id
JOIN read_csv("PCODE_PARENT.facts", header=false) par USING ("column0") --id
JOIN read_csv("BB_HFUNC.facts", header=false) bbf ON (par."column1" = bbf."column0") --bbid
LEFT JOIN read_csv("PCODE_OUTPUT.facts", header=false) o USING ("column0")
JOIN read_csv("PCODE_INPUT.facts", header=false) i0 ON (i0."column0"=idx."column0" AND i0."column1"=0)
LEFT JOIN read_csv("PCODE_INPUT.facts", header=false) i1 ON (i1."column0"=idx."column0" AND i1."column1"=1)
LEFT JOIN read_csv("PCODE_INPUT.facts", header=false) i2 ON (i2."column0"=idx."column0" AND i2."column1"=2)
ORDER BY target."column1", idx."column1";
-- WHERE
-- Function to fetch
-- bbf.hfunc = 'main@1400014d2'
-- ORDER BY target.target_address, idx."index";
```

## Sqlite

```
cd ~/.local/state/ctadl/imports/ls/facts
cat pcode_schema.sql | sqlite3 facts.db
```

