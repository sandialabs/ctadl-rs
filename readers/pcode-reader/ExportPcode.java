/* ###
 * IP: GHIDRA
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
//Decompile every function in the program, then output facts files corresponding to the pcodes
//@category PCode

import java.io.BufferedWriter;
import java.io.File;
import java.io.FileWriter;
import java.io.IOException;
import java.io.Writer;
import java.math.BigInteger;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collection;
import java.util.Collections;
import java.util.EnumMap;
import java.util.EnumSet;
import java.util.HashMap;
import java.util.HashSet;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.Set;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;

import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileOptions;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.decompiler.parallel.DecompileConfigurer;
import ghidra.app.decompiler.parallel.DecompilerCallback;
import ghidra.app.decompiler.parallel.ParallelDecompiler;
import ghidra.app.script.GhidraScript;
import ghidra.framework.plugintool.PluginTool;
import ghidra.graph.GDirectedGraph;
import ghidra.graph.GEdge;
import ghidra.graph.GVertex;
import ghidra.graph.GraphFactory;
import ghidra.graph.algo.DepthFirstSorter;
import ghidra.program.database.symbol.FunctionSymbol;
import ghidra.program.model.address.Address;
import ghidra.program.model.address.AddressFactory;
import ghidra.program.model.address.AddressSpace;
import ghidra.program.model.data.AbstractFloatDataType;
import ghidra.program.model.data.AbstractIntegerDataType;
import ghidra.program.model.data.Array;
import ghidra.program.model.data.BooleanDataType;
import ghidra.program.model.data.DataType;
import ghidra.program.model.data.DataTypeComponent;
import ghidra.program.model.data.FunctionDefinition;
import ghidra.program.model.data.ParameterDefinition;
import ghidra.program.model.data.Pointer;
import ghidra.program.model.data.Structure;
import ghidra.program.model.data.TypeDef;
import ghidra.program.model.data.Union;
import ghidra.program.model.listing.Data;
import ghidra.program.model.listing.DataIterator;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.Program;
import ghidra.program.model.mem.MemoryAccessException;
import ghidra.program.model.pcode.FunctionPrototype;
import ghidra.program.model.pcode.HighConstant;
import ghidra.program.model.pcode.HighFunction;
import ghidra.program.model.pcode.HighGlobal;
import ghidra.program.model.pcode.HighLocal;
import ghidra.program.model.pcode.HighOther;
import ghidra.program.model.pcode.HighSymbol;
import ghidra.program.model.pcode.HighVariable;
import ghidra.program.model.pcode.PcodeBlock;
import ghidra.program.model.pcode.PcodeBlockBasic;
import ghidra.program.model.pcode.PcodeOp;
import ghidra.program.model.pcode.PcodeOpAST;
import ghidra.program.model.pcode.SequenceNumber;
import ghidra.program.model.pcode.Varnode;
import ghidra.program.model.pcode.VarnodeAST;
import ghidra.program.model.symbol.ExternalReference;
import ghidra.program.model.symbol.Reference;
import ghidra.program.model.symbol.Symbol;
import ghidra.program.model.symbol.SymbolIterator;
import ghidra.program.model.symbol.SymbolTable;
import ghidra.program.model.symbol.ThunkReference;
import ghidra.program.model.lang.Register;
import ghidra.program.model.lang.Language;
import ghidra.program.model.lang.CompilerSpec;
import ghidra.util.task.TaskMonitor;

class PcodeBlockBasicVertex implements GVertex {
	private PcodeBlock bb;

	public PcodeBlockBasicVertex(PcodeBlock bb) {
		this.bb = bb;
	}

	public PcodeBlockBasic bb() {
		return (PcodeBlockBasic) bb;
	}
}

class PcodeBlockBasicEdge implements GEdge<PcodeBlockBasicVertex> {
	private PcodeBlockBasicVertex start;
	private PcodeBlockBasicVertex end;

	public PcodeBlockBasicEdge(PcodeBlockBasicVertex start, PcodeBlockBasicVertex end) {
		this.start = start;
		this.end = end;
	}

	public PcodeBlockBasicVertex getStart() {
		return start;
	}

	public PcodeBlockBasicVertex getEnd() {
		return end;
	}
}

class DecompilerConfigurer implements DecompileConfigurer {

	DecompInterface ifc;

	DecompilerConfigurer(Program p) {
	}

	@Override
	public void configure(DecompInterface decompiler) {
		this.ifc = decompiler;
		DecompileOptions options = new DecompileOptions();
		decompiler.setOptions(options);
		decompiler.setSimplificationStyle("decompile"); // can also use "normalize" but that won't generate
														// HighVariables
	}

	DecompInterface getInteface() {
		return ifc;
	}

}

/**
 * The tables this script writes: exactly the ones ctadl's pcode reader reads. Each is written
 * to {@code <name>.facts}.
 *
 * Every entity id is built from small integers, which is most of what keeps the tables small:
 * a function is its number (see {@link HighFunctionExporter#numberFunctions}), and a pcode op,
 * varnode or basic block is that number, a colon, and a number Ghidra gives it within the
 * function. {@code HFUNC_FUNC} and {@code VNODE_NAME} carry the readable names.
 */
enum PredicateFile {
	FACTS_VERSION("FACTS_VERSION"), HFUNC_LOCAL_EP("HFUNC_LOCAL_EP"), HFUNC_FUNC("HFUNC_FUNC"),
	HFUNC_PROTO("HFUNC_PROTO"), HFUNC_ISEXT("HFUNC_ISEXT"), HFUNC_EP("HFUNC_EP"), HFUNC_NAME("HFUNC_NAME"),
	HVAR_CLASS("HVAR_CLASS"), HVAR_TYPE("HVAR_TYPE"), HVAR_REPRESENTATIVE("HVAR_REPRESENTATIVE"),
	PCODE_MNEMONIC("PCODE_MNEMONIC"), PCODE_PARENT("PCODE_PARENT"), PCODE_TARGET("PCODE_TARGET"),
	PCODE_INPUT("PCODE_INPUT"), PCODE_OUTPUT("PCODE_OUTPUT"), BB_PCODE_INDEX("BB_PCODE_INDEX"),
	VNODE_ADDRESS("VNODE_ADDRESS"), VNODE_OFFSET_N("VNODE_OFFSET_N"), VNODE_SIZE("VNODE_SIZE"),
	VNODE_NAME("VNODE_NAME"), VNODE_SPACE("VNODE_SPACE"), VNODE_HVAR("VNODE_HVAR"),
	REGISTER_OFF_NAME("REGISTER_OFF_NAME"), REGISTER_IS_SP("REGISTER_IS_SP"), TYPE_POINTER("TYPE_POINTER"),
	TYPE_BOOLEAN("TYPE_BOOLEAN"), TYPE_INTEGER("TYPE_INTEGER"), TYPE_FLOAT("TYPE_FLOAT"), TYPE_ENUM("TYPE_ENUM"),
	BB_LAST("BB_LAST"), BB_OUT("BB_OUT"), BB_FOUT("BB_FOUT"), BB_TOUT("BB_TOUT"),
	BB_FIRST("BB_FIRST"), BB_HFUNC("BB_HFUNC"), BB_START("BB_START"), PROTO_IS_VARARG("PROTO_IS_VARARG"),
	PROTO_IS_VOID("PROTO_IS_VOID"), PROTO_RETTYPE("PROTO_RETTYPE"), PROTO_PARAMETER("PROTO_PARAMETER"),
	PROTO_PARAMETER_COUNT("PROTO_PARAMETER_COUNT"), SYMBOL_HVAR("SYMBOL_HVAR"),
	PROGRAM_IMAGE_BASE("PROGRAM_IMAGE_BASE");

	private final String name;

	PredicateFile(String name) {
		this.name = name;
	}

	@Override
	public String toString() {
		return name;
	}

	public Writer getWriter(File directory, String suffix, boolean append) throws IOException {
		File factsFile = new File(directory, name + suffix);
		if (!append)
			touch(factsFile);
		return new BufferedWriter(new FileWriter(factsFile, true));
	}

	public static void touch(File file) throws IOException {
		file.delete();
		file.createNewFile();
	}
}

class Database {
	private static final char DBSEP = '\t';
	private static final char EOL = '\n';

	protected final Map<PredicateFile, Collection<List<String>>> contents;
	protected boolean everCleared;

	public Database() {
		contents = new EnumMap<>(PredicateFile.class);
		for (PredicateFile predicateFile : EnumSet.allOf(PredicateFile.class))
			contents.put(predicateFile, new HashSet<List<String>>());
		everCleared = false;
	}

	public void writeFile(Collection<List<String>> contents, PredicateFile predicateFile, String directory)
			throws IOException {
		Writer w = predicateFile.getWriter(new File(directory), ".facts", everCleared);
		for (List<String> record : contents) {
			writeRecord(w, record);
		}
		w.flush();
		w.close();
	}

	public void writeFacts(String directory, boolean writeAll) throws IOException {
		final int k = Runtime.getRuntime().availableProcessors() * 2 - 1;
		ExecutorService es = Executors.newFixedThreadPool(k);
		for (PredicateFile predicateFile : EnumSet.allOf(PredicateFile.class)) {
			if (!writeAll && predicateFile.name().startsWith("TYPE_")) {
				continue;
			}
			es.execute(() -> {
				try {
					writeFile(this.contents.get(predicateFile), predicateFile, directory);
				} catch (Exception e) {
					ghidra.util.Msg.info(this, e);
					for (java.lang.StackTraceElement st : e.getStackTrace()) {
						ghidra.util.Msg.info(this, st);
					}
				}
			});
		}

		es.shutdown();
		try {
			while (!es.awaitTermination(30, TimeUnit.SECONDS))
				;
		} catch (Exception e) {
			ghidra.util.Msg.info(this, e);
			for (java.lang.StackTraceElement st : e.getStackTrace()) {
				ghidra.util.Msg.info(this, st);
			}
		}

		this.clear(writeAll);
	}

	private void writeRecord(Writer writer, List<String> record) throws IOException {
		boolean first = true;
		for (String col : record) {
			if (!first) {
				writer.write(DBSEP);
			}
			writeColumn(writer, col);
			first = false;
		}
		writer.write(EOL);
	}

	private void writeColumn(Writer writer, String column) throws IOException {
		// Quote some special characters.
		final char QUOTE = '\"';
		final char SLASH = '\\';
		final char TABCH = '\t';
		for (int i = 0; i < column.length(); i++) {
			char c = column.charAt(i);
			switch (c) {
			case QUOTE:
				writer.write("'");
				break;
			case SLASH:
				writer.write('\\');
				writer.write('\\');
				break;
			case EOL:
				writer.write('\\');
				writer.write('n');
				break;
			case TABCH:
				writer.write('\\');
				writer.write('t');
				break;
			default:
				writer.write(c);
			}
		}
	}

	public void add(PredicateFile predicateFile, String... args) {
		this.contents.get(predicateFile).add(Arrays.asList(args));
	}

	public void merge(Database other) {
		for (Map.Entry<PredicateFile, Collection<List<String>>> entry : other.contents.entrySet()) {
			this.contents.get(entry.getKey()).addAll(entry.getValue());
		}

	}

	public void clear(boolean clearAll) {
		for (PredicateFile predicateFile : EnumSet.allOf(PredicateFile.class)) {
			if (!clearAll && predicateFile.name().startsWith("TYPE_")) {
				continue;
			}
			contents.put(predicateFile, new HashSet<List<String>>());
		}
		everCleared = true;
	}
}


class HighFunctionExporter {
	/**
	 * The version of the tables' format, written to {@code FACTS_VERSION}. ctadl reads only
	 * tables of the version it expects, so raise it whenever a table changes meaning.
	 */
	static final String FACTS_VERSION = "2";

	private final Database db = new Database();
	private final Set<String> types = new HashSet<String>();
	private Set<String> varnodes = new HashSet<String>();
	private Map<HighVariable, VarnodeAST> extraGlobals = new HashMap<HighVariable, VarnodeAST>();
	private final String directory;
	/** Each function's number, its id in the tables. See {@link #numberFunctions}. */
	private final Map<Address, Integer> functionNumbers = new HashMap<Address, Integer>();

	String SEP = ":";

	public HighFunctionExporter(String directory) throws IOException {
		this.directory = directory;
	}

	/**
	 * Numbers {@code functions} in order of entry point, so that a function's id is the same
	 * whatever order the decompiler finishes functions in.
	 */
	public void numberFunctions(Collection<Function> functions) {
		List<Address> entries = new ArrayList<Address>();
		for (Function f : functions) {
			entries.add(f.getEntryPoint());
		}
		Collections.sort(entries);
		for (Address entry : entries) {
			functionNumbers.putIfAbsent(entry, functionNumbers.size());
		}
	}

	public void writeFacts(boolean writeAll) throws IOException {
		db.writeFacts(directory, writeAll);
	}

	public Database getDatabase() {
		return db;
	}

	public void processFunction(DecompileResults res, Function f, DecompInterface decompiler) {
		varnodes = new HashSet<String>();
		extraGlobals = new HashMap<HighVariable, VarnodeAST>();

// TODO: This should only be done once if dumping the entire program
		SymbolIterator externalSymbols = f.getProgram().getSymbolTable().getSymbols(f.getName());
		while (externalSymbols.hasNext()) {
			Symbol next = externalSymbols.next();
			if (!next.isExternal()) {
				Address address = next.getAddress();
				export(PredicateFile.HFUNC_LOCAL_EP, addressString(f.getEntryPoint()), addressString(address));
			} else if (next instanceof FunctionSymbol fsym) {
				Reference[] references = fsym.getReferences();
				for (Reference r : references) {
					if (r instanceof ThunkReference || r instanceof ExternalReference) {
						Address address = r.getFromAddress();
						if (address != null) {
							export(PredicateFile.HFUNC_LOCAL_EP, addressString(f.getEntryPoint()),
									addressString(address));
						}
					}
				}
			}
		}
		HighFunction high = getHighFunction(res, f, decompiler);

		// we want to make sure the pcode indexes are in source order.
		// we accomplish this by sorting the basic blocks topologically.
		GDirectedGraph<PcodeBlockBasicVertex, PcodeBlockBasicEdge> g = GraphFactory.createDirectedGraph();
		for (PcodeBlockBasic bb : high.getBasicBlocks()) {
			PcodeBlockBasicVertex v = new PcodeBlockBasicVertex(bb);
			g.addVertex(v);
			for (int i = 0; i < bb.getInSize(); i++) {
				PcodeBlockBasicVertex vIn = new PcodeBlockBasicVertex(bb.getIn(i));
				g.addVertex(vIn);
				g.addEdge(new PcodeBlockBasicEdge(vIn, v));
			}
			for (int i = 0; i < bb.getOutSize(); i++) {
				PcodeBlockBasicVertex vOut = new PcodeBlockBasicVertex(bb.getOut(i));
				g.addVertex(vOut);
				g.addEdge(new PcodeBlockBasicEdge(v, vOut));
			}
		}

		HashSet<PcodeOp> set = new HashSet<PcodeOp>();
		HashSet<Integer> bbset = new HashSet<Integer>();
		for (PcodeBlockBasicVertex v : DepthFirstSorter.preOrder(g)) {
			PcodeBlockBasic bb = v.bb();
			if (bbset.contains(bb.getIndex())) {
				continue;
			}
			bbset.add(bb.getIndex());
			int bbIndex = 0;
			Iterator<PcodeOp> opiter = bb.getIterator();
			while (opiter.hasNext()) {
				PcodeOp op = opiter.next();
				if (op != null) {
					set.add(op);
					exportPcode(high, op);
					exportN(PredicateFile.BB_PCODE_INDEX, bbID(high, bb), bbIndex++, pcodeID(high, op));
				}
			}
		}

		exportPcodeOpSequence(high, set);
		exportHighFunction(high);
	}

	private HighFunction getHighFunction(DecompileResults res, Function func, DecompInterface decompiler) {
		HighFunction high = res.getHighFunction();
		if (high == null) {
			high = new HighFunction(func, decompiler.getLanguage(), decompiler.getCompilerSpec(),
					decompiler.getDataTypeManager());
			int default_extrapop = decompiler.getCompilerSpec().getDefaultCallingConvention().getExtrapop();
			high.grabFromFunction(default_extrapop, false, false);
			String id = funcID(func);
			export(PredicateFile.HFUNC_ISEXT, id);
		}
		return high;
	}

	private String addressString(Address addr) {
		return Long.toString(addr.getOffset());
	}

	private void export(PredicateFile pfile, String key) {
		db.add(pfile, key);
	}

	private void export(PredicateFile pfile, String key, String value) {
		db.add(pfile, key, value);
	}

	@SuppressWarnings("unused")
	private void export(PredicateFile pfile, String key, String val1, String val2) {
		db.add(pfile, key, val1, val2);
	}

	private void exportN(PredicateFile label, String key, int index, String value) {
		if (value.equals("")) {
			db.add(label, key, Integer.toString(index));
		} else {
			db.add(label, key, Integer.toString(index), value);
		}
	}

	private void exportPcode(HighFunction hfn, PcodeOp op) {
		String id = pcodeID(hfn, op);
		export(PredicateFile.PCODE_MNEMONIC, id, op.getMnemonic());
		export(PredicateFile.PCODE_PARENT, id, bbID(hfn, op.getParent()));
		export(PredicateFile.PCODE_TARGET, id, addressString(op.getSeqnum().getTarget()));
		for (int i = 0; i < op.getNumInputs(); ++i) {
			VarnodeAST vni = (VarnodeAST) op.getInput(i);
			if (vni != null) {
				// OK, this is a little weird, but PTRSUBs with first arg == 0
				// are (usually) global variables at address == second arg
				if (op.getMnemonic().equals("PTRSUB") && (i == 0)) {
					if (vni.getAddress().getOffset() == 0) {
						VarnodeAST next = (VarnodeAST) op.getInput(1);
						HighVariable high = next.getHigh();
						if (high != null) {
							extraGlobals.put(high, next);
						}
					}
				}
				exportN(PredicateFile.PCODE_INPUT, id, i, vnodeID(hfn, vni));
				exportVarnode(hfn, vni);
			}
		}
		VarnodeAST vno = (VarnodeAST) op.getOutput();
		if (vno != null) {
			export(PredicateFile.PCODE_OUTPUT, id, vnodeID(hfn, vno));
			exportVarnode(hfn, vno);
		}
	}

	private void exportVarnode(HighFunction hfn, VarnodeAST vn) {
		String id = vnodeID(hfn, vn);
		if (!varnodes.add(id))
			return;
		HighVariable hv = vn.getHigh();
		// The varnode's readable name, without its function: its high variable's name and its
		// number. Taken before anything below can change the high variable's name.
		String name = Integer.toString(vn.getUniqueId());
		if (hv != null) {
			name = hvarName(hfn, hv) + SEP + name;
		}
		export(PredicateFile.VNODE_NAME, id, name);
		export(PredicateFile.VNODE_ADDRESS, id, addressString(vn.getAddress()));
		long offset = vn.getOffset();
		export(PredicateFile.VNODE_OFFSET_N, id, Long.toString(offset));
		export(PredicateFile.VNODE_SIZE, id, Integer.toString(vn.getSize()));
		export(PredicateFile.VNODE_SPACE, id, vn.getAddress().getAddressSpace().getName());
		if (hv != null) {
			if (hv instanceof HighConstant && hv.getDataType() instanceof Pointer) {
				if (offset != 0) {
					extraGlobals.put(hv, vn);
				}
			}
			export(PredicateFile.VNODE_HVAR, id, hvarID(hfn, hv));
			exportHighVariable(hfn, hv, true);
		}
	}

	private void exportHighVariable(HighFunction hfn, HighVariable hv, boolean dontDescend) {
		String id = hvarID(hfn, hv);
		if (hv instanceof HighGlobal) {
			export(PredicateFile.HVAR_CLASS, id, "global");
		}
		if (hv instanceof HighLocal) {
			export(PredicateFile.HVAR_CLASS, id, "local");
		}
		if (hv instanceof HighConstant) {
			export(PredicateFile.HVAR_CLASS, id, "constant");
		}
		if (hv instanceof HighOther) {
			export(PredicateFile.HVAR_CLASS, id, "other");
		}
		DataType dataType = hv.getDataType();
		if (dataType != null) {
			export(PredicateFile.HVAR_TYPE, id, dtID(dataType));
			exportType(dataType);
		}
		if (hv.getSymbol() != null) {
			export(PredicateFile.SYMBOL_HVAR, hsID(hfn, hv.getSymbol()), id);
		}
		if (!dontDescend) {
			VarnodeAST representative = (VarnodeAST) hv.getRepresentative();
			if (representative != null) {
				export(PredicateFile.HVAR_REPRESENTATIVE, id, vnodeID(hfn, representative));
				exportVarnode(hfn, representative);
			}
			Varnode[] instances = hv.getInstances();
			for (Varnode varnode : instances) {
				exportVarnode(hfn, (VarnodeAST) varnode);
			}
		}
	}

	/** Records whether the type `dataType` names, through its typedefs, is a pointer or scalar. */
	private void exportType(DataType dataType) {
		String id = dtID(dataType);
		if (!types.add(id))
			return;

		while (dataType instanceof TypeDef) {
			TypeDef typedef = (TypeDef) dataType;
			dataType = typedef.getBaseDataType();
		}
		if (dataType instanceof Pointer) {
			export(PredicateFile.TYPE_POINTER, id);
		}
		if (dataType instanceof BooleanDataType) {
			export(PredicateFile.TYPE_BOOLEAN, id);
		}
		if (dataType instanceof AbstractIntegerDataType) {
			export(PredicateFile.TYPE_INTEGER, id);
		}
		if (dataType instanceof AbstractFloatDataType) {
			export(PredicateFile.TYPE_FLOAT, id);
		}
		if (dataType instanceof Enum) {
			export(PredicateFile.TYPE_ENUM, id);
		}
	}

	private void exportHighFunction(HighFunction hfn) {
		Function f = hfn.getFunction();

		for (PcodeBlockBasic bb : hfn.getBasicBlocks()) {
			String bbid = bbID(hfn, bb);
			export(PredicateFile.BB_HFUNC, bbid, hfuncID(hfn));
			if (bb.getStart() != null) {
				export(PredicateFile.BB_START, bbid, addressString(bb.getStart()));
			}
			for (int i = 0; i < bb.getOutSize(); i++) {
				export(PredicateFile.BB_OUT, bbid, bbID(hfn, bb.getOut(i)));
			}
			if (bb.getOutSize() > 1) {
				export(PredicateFile.BB_TOUT, bbid, bbID(hfn, bb.getTrueOut()));
				export(PredicateFile.BB_FOUT, bbid, bbID(hfn, bb.getFalseOut()));
			}
		}
		String id = hfuncID(hfn);
		export(PredicateFile.HFUNC_NAME, id, hfn.getFunction().getName());
		export(PredicateFile.HFUNC_FUNC, id, funcName(hfn.getFunction()));
		export(PredicateFile.HFUNC_EP, id, addressString(hfn.getFunction().getEntryPoint()));
		FunctionPrototype proto = hfn.getFunctionPrototype();
		if (proto != null) {
			// A function's prototype is identified by the function.
			export(PredicateFile.HFUNC_PROTO, id, id);
			exportPrototype(hfn, proto, id);
		}
		Function thunk = f.getThunkedFunction(false);
		if (thunk != null && thunk.isExternal()) {
			export(PredicateFile.HFUNC_ISEXT, id);
		}

		Iterator<HighSymbol> symbols = hfn.getLocalSymbolMap().getSymbols();
		while (symbols.hasNext()) {
			HighSymbol hs = symbols.next();
			HighVariable hv = hs.getHighVariable();
			if (hv != null) {
				export(PredicateFile.SYMBOL_HVAR, hsID(hfn, hs), hvarID(hfn, hv));
				exportHighVariable(hfn, hv, false);
			}
		}
	}

	public void exportRegisterInfo(Program program) {
		Language language = program.getLanguage();
		CompilerSpec cSpec = program.getCompilerSpec();
		Register sp = cSpec.getStackPointer();

		for (Register reg : language.getRegisters()) {
			if (reg.getAddress().isRegisterAddress()) {
				export(PredicateFile.REGISTER_OFF_NAME, Long.toString(reg.getAddress().getOffset()),
						Integer.toString(reg.getMinimumByteSize()), reg.getName());
				if (sp != null && (sp.equals(reg) || sp.contains(reg))) {
					export(PredicateFile.REGISTER_IS_SP, reg.getName());
				}
			}
		}
	}

	private void exportPrototype(HighFunction hfn, FunctionPrototype proto, String id) {
		exportN(PredicateFile.PROTO_PARAMETER_COUNT, id, proto.getNumParams(), "");
		for (int i = 0; i < proto.getNumParams(); i++) {
			HighSymbol param = proto.getParam(i);
			String hsid = hsID(hfn, param);
			HighVariable pvar = param.getHighVariable();
			if (pvar != null) {
				exportHighVariable(hfn, pvar, true);
				export(PredicateFile.SYMBOL_HVAR, hsid, hvarID(hfn, pvar));
				VarnodeAST rep = (VarnodeAST) pvar.getRepresentative();
				export(PredicateFile.HVAR_REPRESENTATIVE, hvarID(hfn, pvar), vnodeID(hfn, rep));
				exportVarnode(hfn, rep);
			} else {
				// fake out a HighVar, with a varnode whose id cannot be a real varnode's: those
				// have one colon.
				String vnid = hsid + SEP + "0";
				export(PredicateFile.SYMBOL_HVAR, hsid, hsid);
				export(PredicateFile.HVAR_REPRESENTATIVE, hsid, vnid);
				export(PredicateFile.VNODE_HVAR, vnid, hsid);
				export(PredicateFile.VNODE_SPACE, vnid, "fake");
				export(PredicateFile.VNODE_NAME, vnid, param.getName() + SEP + "0");
			}
			exportN(PredicateFile.PROTO_PARAMETER, id, i, hsid);
		}
		DataType returnType = proto.getReturnType();
		if (returnType != null) {
			export(PredicateFile.PROTO_RETTYPE, id, dtID(returnType));
		}
		if (proto.hasNoReturn())
			export(PredicateFile.PROTO_IS_VOID, id);
		if (proto.isVarArg())
			export(PredicateFile.PROTO_IS_VARARG, id);
	}

	private void exportPcodeOpSequence(HighFunction hfn, HashSet<PcodeOp> set) {
		Iterator<PcodeOpAST> opiter = hfn.getPcodeOps();
		HashSet<PcodeBlockBasic> seenParents = new HashSet<PcodeBlockBasic>();
		HashMap<PcodeBlock, PcodeOp> first = new HashMap<PcodeBlock, PcodeOp>();
		HashMap<PcodeBlock, PcodeOp> last = new HashMap<PcodeBlock, PcodeOp>();
		while (opiter.hasNext()) {
			PcodeOpAST op = opiter.next();
			PcodeBlockBasic parent = op.getParent();
			if (seenParents.contains(parent)) {
				continue;
			}
			Iterator<PcodeOp> iterator = parent.getIterator();
			PcodeOp prev = null;
			PcodeOp next = null;
			while (iterator.hasNext()) {
				next = iterator.next();
				if (prev == null && set.contains(next)) {
					first.put(parent, next);
				}
				prev = next;
			}
			if (next != null && set.contains(next)) {
				last.put(parent, next);
			}
			seenParents.add(parent);
		}
		for (PcodeBlock block : first.keySet()) {
			PcodeOpAST ast = (PcodeOpAST) first.get((block));
			export(PredicateFile.BB_FIRST, bbID(hfn, block), pcodeID(hfn, ast));
		}
		for (PcodeBlock block : last.keySet()) {
			export(PredicateFile.BB_LAST, bbID(hfn, block), pcodeID(hfn, last.get(block)));
		}
	}

	/** A pcode op: its function and its sequence number's time, which is unique in the function. */
	private String pcodeID(HighFunction hfn, PcodeOp op) {
		SequenceNumber sn = op.getSeqnum();
		if (sn != null) {
			return hfuncID(hfn) + SEP + sn.getTime();
		}
		return hfuncID(hfn) + SEP + "NO_SEQNUM" + SEP + op.toString();
	}

	/** A varnode: its function and its unique id, which is unique in the function. */
	private String vnodeID(HighFunction hfn, VarnodeAST vn) {
		return hfuncID(hfn) + SEP + Integer.toString(vn.getUniqueId());
	}

	private String hvarID(HighFunction hfn, HighVariable hv) {
		return hfuncID(hfn) + SEP + hvarName(hfn, hv);
	}

	private String hvarName(HighFunction hf, HighVariable hv) {
		if (hv.getName() == null || hv.getName().equals("UNNAMED")) {
			SymbolTable symbolTable = hf.getFunction().getProgram().getSymbolTable();
			Varnode rep = hv.getRepresentative();
			Address addr = rep.getAddress();
			if (extraGlobals.containsKey(hv)) {
				VarnodeAST vn = extraGlobals.get(hv);
				addr = addr.getNewAddress(vn.getOffset());
			}
			Symbol symbol = symbolTable.getPrimarySymbol(addr);
			if (symbol == null) {
				return addr.toString();
			}
			export(PredicateFile.HVAR_CLASS, hfuncID(hf) + SEP + symbol.getName(), "global");
			return symbol.getName();
		}
		return hv.getName();
	}

	private String hsID(HighFunction hfn, HighSymbol hs) {
		return hfuncID(hfn) + SEP + hs.getName();
	}

	/** A function's id: its number. See {@link #numberFunctions}. */
	public String funcID(Function fn) {
		Integer number = functionNumbers.get(fn.getEntryPoint());
		if (number == null) {
			// Not one of the functions numbered up front. Not expected, but give it a number
			// none of those has.
			number = functionNumbers.size();
			functionNumbers.put(fn.getEntryPoint(), number);
		}
		return Integer.toString(number);
	}

	/** A function's readable name: its name, with namespace, at its entry point. */
	public String funcName(Function fn) {
		return fn.getName(true) + "@" + fn.getEntryPoint();
	}

	public String hfuncID(HighFunction fn) {
		Function f = fn.getFunction();
		return funcID(f);
	}

	/** A basic block: its function and its start address. */
	private String bbID(HighFunction hfn, PcodeBlock bb) {
		if (bb.getStart() != null) {
			return hfuncID(hfn) + SEP + addressString(bb.getStart());
		}
		return hfuncID(hfn) + SEP + "unknown";
	}

	private String dtID(DataType dt) {
		if (dt.getName() != null) {
			return dt.getName().replaceAll(" ", "");
		}
		return dt.toString();
	}
}

public class ExportPcode extends GhidraScript {

	File outputDirectory;

	@Override
	protected void run() throws Exception {

		String[] args = getScriptArgs();
		PluginTool tool = state.getTool();
		if (tool == null) {
			println("Script is not running in GUI");
		}
		if (args.length >= 1) {
			outputDirectory = new File(args[0]);
		} else {
			outputDirectory = askDirectory("Select Directory for Results", "OK");
		}

		long startTime = System.nanoTime();

		HighFunctionExporter ex = new HighFunctionExporter(outputDirectory.getAbsolutePath());
		ex.getDatabase().add(PredicateFile.FACTS_VERSION, HighFunctionExporter.FACTS_VERSION);
		// The base address Ghidra loaded this program at. Instruction addresses in
		// the facts are absolute (include this base); subtracting it recovers the
		// section-relative offset used by addr2line/DWARF.
		ex.getDatabase().add(PredicateFile.PROGRAM_IMAGE_BASE,
				Long.toString(currentProgram.getImageBase().getOffset()));

		DecompilerConfigurer configurer = new DecompilerConfigurer(currentProgram);

		DecompilerCallback<DecompileResults> callback = new DecompilerCallback<DecompileResults>(currentProgram,
				configurer) {
			int count = 0;

			@Override
			// This could be done better, when results are available, other decompiler
			// results will stall.
			public synchronized DecompileResults process(DecompileResults results, TaskMonitor tMonitor)
					throws Exception {
				ex.processFunction(results, results.getFunction(), configurer.getInteface());
				count++;
				if (count > 20) {
					ex.writeFacts(false);
					count = 0;
				}
				return null;
			}
		};

		Set<Function> toProcess = new HashSet<Function>();
		currentProgram.getFunctionManager().getFunctions(true).forEach(f -> {
			toProcess.add(f);
		});
		currentProgram.getFunctionManager().getExternalFunctions().forEach(f -> {
			toProcess.add(f);
		});
		List<Function> toProcessList = new ArrayList<Function>(toProcess);
		ex.numberFunctions(toProcessList);

		Collections.sort(toProcessList,
				(o1, o2) -> -Long.compare(o1.getBody().getNumAddresses(), o2.getBody().getNumAddresses()));

		monitor.initialize(toProcess.size());

		ex.exportRegisterInfo(currentProgram);
		ex.writeFacts(true);

		ParallelDecompiler.decompileFunctions(callback, toProcessList, monitor);

		// waiting until the end to write all datatypes TYPE_ may not save much time.
		// It could save time if the structures are complicated, so constantly
		// traversing
		// structure references all over again every 50 functions. Might not be worth
		// it.
		ex.writeFacts(true);

		long endTime = System.nanoTime();
		long duration = (endTime - startTime) / 1000000;
		println("total duration: " + Long.toString(duration));
	}

}
