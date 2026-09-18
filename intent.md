Importing Android code that has native code inside of it normally co-indexes everything. This loads all the code into one indexing job and retains precision when analyzing both sides of JNI interactions. The problem is, for larg apps, this takes too many resources. This analyzer is compositional, so let's leverage that by building the ability to index the native code (sub-indexes) each independently, then loading their function summaries as models into the indexing step of the apk itself.

Add a CLI flag so that sub-imports are indexed compositionally. As said above, the canonical case for this is Android + native code, but it really applies to anything that has sub-imports.

The goal is to independently index each sub-import, producing an index which is saved like normal. When analyzing the app code, only load the summaries from the sub-index (not the whole index) and make sure to map them properly into the app code. For JNI code using name mangling or RegisterNatives, this means you'll have to apply that matching when loading the summaries. 


Free RAM resources after each sub-index. They're independent so this should be easy.

Use existing APIs where applicable, specifically when reading and writing to the store.
