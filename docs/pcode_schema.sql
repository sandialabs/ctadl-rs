-- cat pcode_schema.sql | sqlite3 facts.db
.mode tabs
create table bb_first(id TEXT, value TEXT);
.import BB_FIRST.facts bb_first

create table bb_fout(id TEXT, out_block TEXT);
.import BB_FOUT.facts bb_fout

create table bb_hfunc(id TEXT, hfunc TEXT);
.import BB_HFUNC.facts bb_hfunc

create table bb_last(id TEXT, pcode TEXT);
.import BB_LAST.facts bb_last

create table bb_out(id TEXT, out_block TEXT);
.import BB_OUT.facts bb_out

create table bb_pcode_index(id TEXT, idx TEXT, pcode TEXT);
.import BB_PCODE_INDEX.facts bb_pcode_index

create table bb_start(id TEXT, address TEXT);
.import BB_START.facts bb_start

create table bb_tout(id TEXT, tout_block TEXT);
.import BB_TOUT.facts bb_tout

create table hfunc_ep(hfunc TEXT, address TEXT);
.import HFUNC_EP.facts hfunc_ep

create table hfunc_func(hfunc TEXT, name TEXT);
.import HFUNC_FUNC.facts hfunc_func

create table hfunc_isext(hfunc TEXT);
.import HFUNC_ISEXT.facts hfunc_isext

create table hfunc_local_ep(addr1 TEXT, addr2 TEXT);
.import HFUNC_LOCAL_EP.facts hfunc_local_ep

create table hfunc_name(hfunc TEXT, name TEXT);
.import HFUNC_NAME.facts hfunc_name

create table hfunc_proto(hfunc TEXT, proto TEXT);
.import HFUNC_PROTO.facts hfunc_proto

create table hvar_class(hvar TEXT, class TEXT);
.import HVAR_CLASS.facts hvar_class

create table hvar_representative(hvar TEXT, representative TEXT);
.import HVAR_REPRESENTATIVE.facts hvar_representative

create table hvar_type(hvar TEXT, type TEXT);
.import HVAR_TYPE.facts hvar_type

create table pcode_input(pcode TEXT, idx TEXT, vnode TEXT);
.import PCODE_INPUT.facts pcode_input

create table pcode_mnemonic(pcode TEXT, mnemonic TEXT);
.import PCODE_MNEMONIC.facts pcode_mnemonic

create table pcode_output(pcode TEXT, vnode TEXT);
.import PCODE_OUTPUT.facts pcode_output

create table pcode_parent(pcode TEXT, bb TEXT);
.import PCODE_PARENT.facts pcode_parent

create table pcode_target(pcode TEXT, address TEXT);
.import PCODE_TARGET.facts pcode_target

create table proto_is_vararg(proto TEXT);
.import PROTO_IS_VARARG.facts proto_is_vararg

create table proto_is_void(proto TEXT);
.import PROTO_IS_VOID.facts proto_is_void

create table proto_parameter_count(proto TEXT, count TEXT);
.import PROTO_PARAMETER_COUNT.facts proto_parameter_count

create table proto_parameter(proto TEXT, idx TEXT, hvar TEXT);
.import PROTO_PARAMETER.facts proto_parameter

create table proto_rettype(proto TEXT, type TEXT);
.import PROTO_RETTYPE.facts proto_rettype

create table register_is_sp(register TEXT);
.import REGISTER_IS_SP.facts register_is_sp

create table register_off_name(offset TEXT, size TEXT, name TEXT);
.import REGISTER_OFF_NAME.facts register_off_name

create table symbol_hvar(symbol TEXT, hvar TEXT);
.import SYMBOL_HVAR.facts symbol_hvar

create table type_boolean(type TEXT);
.import TYPE_BOOLEAN.facts type_boolean

create table type_enum(type TEXT);
.import TYPE_ENUM.facts type_enum

create table type_float(type TEXT);
.import TYPE_FLOAT.facts type_float

create table type_integer(type TEXT);
.import TYPE_INTEGER.facts type_integer

create table type_pointer(type TEXT);
.import TYPE_POINTER.facts type_pointer

create table vnode_address(vnode TEXT, address TEXT);
.import VNODE_ADDRESS.facts vnode_address

create table vnode_hvar(vnode TEXT, hvar TEXT);
.import VNODE_HVAR.facts vnode_hvar

create table vnode_name(vnode TEXT, name TEXT);
.import VNODE_NAME.facts vnode_name

create table vnode_offset_n(vnode TEXT, offset TEXT);
.import VNODE_OFFSET_N.facts vnode_offset_n

create table vnode_size(vnode TEXT, size TEXT);
.import VNODE_SIZE.facts vnode_size

create table vnode_space(vnode TEXT, space TEXT);
.import VNODE_SPACE.facts vnode_space

create table facts_version(version TEXT);
.import FACTS_VERSION.facts facts_version

create table program_image_base(address TEXT);
.import PROGRAM_IMAGE_BASE.facts program_image_base
