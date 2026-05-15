-- cat pcode_schema.sql | sqlite3 facts.db
.mode tabs
create table bb_first(id TEXT, value TEXT);
.import BB_FIRST.facts bb_first

create table bb_fout(id TEXT, out_block TEXT);
.import BB_FOUT.facts bb_fout

create table bb_hfunc(id TEXT, hfunc TEXT);
.import BB_HFUNC.facts bb_hfunc

create table bb_in(id TEXT, in_block TEXT);
.import BB_IN.facts bb_in

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

create table ctadllanguage(language TEXT);
.import CTADLLanguage.facts ctadllanguage

create table data_string(address TEXT, value TEXT);
.import DATA_STRING.facts data_string

create table hfunc_cspec(hfunc TEXT, cspec TEXT);
.import HFUNC_CSPEC.facts hfunc_cspec

create table hfunc_ep(hfunc TEXT, address TEXT);
.import HFUNC_EP.facts hfunc_ep

create table hfunc_func(hfunc TEXT, func_id TEXT);
.import HFUNC_FUNC.facts hfunc_func

create table hfunc_isep(hfunc TEXT);
.import HFUNC_ISEP.facts hfunc_isep

create table hfunc_isext(hfunc TEXT);
.import HFUNC_ISEXT.facts hfunc_isext

create table hfunc_lang(hfunc TEXT, lang TEXT);
.import HFUNC_LANG.facts hfunc_lang

create table hfunc_local_ep(addr1 TEXT, addr2 TEXT);
.import HFUNC_LOCAL_EP.facts hfunc_local_ep

create table hfunc_name(hfunc TEXT, name TEXT);
.import HFUNC_NAME.facts hfunc_name

create table hfunc_proto(hfunc TEXT, proto TEXT);
.import HFUNC_PROTO.facts hfunc_proto

create table hfunc_tostr(hfunc TEXT, tostr TEXT);
.import HFUNC_TOSTR.facts hfunc_tostr

create table hvar_class(hvar TEXT, class TEXT);
.import HVAR_CLASS.facts hvar_class

create table hvar_name(hvar TEXT, name TEXT);
.import HVAR_NAME.facts hvar_name

create table hvar_representative(hvar TEXT, representative TEXT);
.import HVAR_REPRESENTATIVE.facts hvar_representative

create table hvar_scope(hvar TEXT, scope TEXT);
.import HVAR_SCOPE.facts hvar_scope

create table hvar_size(hvar TEXT, size TEXT);
.import HVAR_SIZE.facts hvar_size

create table hvar_type(hvar TEXT, type TEXT);
.import HVAR_TYPE.facts hvar_type

create table offset_index(offset TEXT, idx TEXT);
.import OFFSET_INDEX.facts offset_index

create table pcode_index(pcode TEXT, idx TEXT);
.import PCODE_INDEX.facts pcode_index

create table pcode_input_count(pcode TEXT, count TEXT);
.import PCODE_INPUT_COUNT.facts pcode_input_count

create table pcode_input(pcode TEXT, idx TEXT, vnode TEXT);
.import PCODE_INPUT.facts pcode_input

create table pcode_mnemonic(pcode TEXT, mnemonic TEXT);
.import PCODE_MNEMONIC.facts pcode_mnemonic

create table pcode_next(pcode TEXT, next_pcode TEXT);
.import PCODE_NEXT.facts pcode_next

create table pcode_opcode(pcode TEXT, opcode TEXT);
.import PCODE_OPCODE.facts pcode_opcode

create table pcode_output(pcode TEXT, vnode TEXT);
.import PCODE_OUTPUT.facts pcode_output

create table pcode_parent(pcode TEXT, bb TEXT);
.import PCODE_PARENT.facts pcode_parent

create table pcode_target(pcode TEXT, address TEXT);
.import PCODE_TARGET.facts pcode_target

create table pcode_time(pcode TEXT, time TEXT);
.import PCODE_TIME.facts pcode_time

create table pcode_tostr(pcode TEXT, tostr TEXT);
.import PCODE_TOSTR.facts pcode_tostr

create table pcodeops(line TEXT);
.import PcodeOps.facts pcodeops

create table program_file(path TEXT);
.import PROGRAM_FILE.facts program_file

create table proto_calling_convention(proto TEXT, convention TEXT);
.import PROTO_CALLING_CONVENTION.facts proto_calling_convention

create table proto_has_this(proto TEXT);
.import PROTO_HAS_THIS.facts proto_has_this

create table proto_is_constructor(proto TEXT);
.import PROTO_IS_CONSTRUCTOR.facts proto_is_constructor

create table proto_is_destructor(proto TEXT);
.import PROTO_IS_DESTRUCTOR.facts proto_is_destructor

create table proto_is_inline(proto TEXT);
.import PROTO_IS_INLINE.facts proto_is_inline

create table proto_is_vararg(proto TEXT);
.import PROTO_IS_VARARG.facts proto_is_vararg

create table proto_is_void(proto TEXT);
.import PROTO_IS_VOID.facts proto_is_void

create table proto_parameter_count(proto TEXT, count TEXT);
.import PROTO_PARAMETER_COUNT.facts proto_parameter_count

create table proto_parameter_datatype(param TEXT, type TEXT);
.import PROTO_PARAMETER_DATATYPE.facts proto_parameter_datatype

create table proto_parameter(proto TEXT, idx TEXT, hvar TEXT);
.import PROTO_PARAMETER.facts proto_parameter

create table proto_rettype(proto TEXT, type TEXT);
.import PROTO_RETTYPE.facts proto_rettype

create table register_is_sp(register TEXT);
.import REGISTER_IS_SP.facts register_is_sp

create table register_off_name(offset TEXT, size TEXT, name TEXT);
.import REGISTER_OFF_NAME.facts register_off_name

create table symbol_hfunc(symbol TEXT, hfunc TEXT);
.import SYMBOL_HFUNC.facts symbol_hfunc

create table symbol_hvar(symbol TEXT, hvar TEXT);
.import SYMBOL_HVAR.facts symbol_hvar

create table symbol_name(symbol TEXT, name TEXT);
.import SYMBOL_NAME.facts symbol_name

create table type_array_base(type TEXT, base_type TEXT);
.import TYPE_ARRAY_BASE.facts type_array_base

create table type_array_element_length(type TEXT, length TEXT);
.import TYPE_ARRAY_ELEMENT_LENGTH.facts type_array_element_length

create table type_array_n(type TEXT, n TEXT);
.import TYPE_ARRAY_N.facts type_array_n

create table type_array(type TEXT);
.import TYPE_ARRAY.facts type_array

create table type_boolean(type TEXT);
.import TYPE_BOOLEAN.facts type_boolean

create table type_enum(type TEXT);
.import TYPE_ENUM.facts type_enum

create table type_float(type TEXT);
.import TYPE_FLOAT.facts type_float

create table type_func_param_count(type TEXT, count TEXT);
.import TYPE_FUNC_PARAM_COUNT.facts type_func_param_count

create table type_func_param(type TEXT, idx TEXT, param_type TEXT);
.import TYPE_FUNC_PARAM.facts type_func_param

create table type_func_ret(type TEXT, ret_type TEXT);
.import TYPE_FUNC_RET.facts type_func_ret

create table type_func_varargs(type TEXT);
.import TYPE_FUNC_VARARGS.facts type_func_varargs

create table type_func(type TEXT);
.import TYPE_FUNC.facts type_func

create table type_integer(type TEXT);
.import TYPE_INTEGER.facts type_integer

create table type_length(type TEXT, length TEXT);
.import TYPE_LENGTH.facts type_length

create table type_name(type TEXT, name TEXT);
.import TYPE_NAME.facts type_name

create table type_pointer_base(type TEXT, base_type TEXT);
.import TYPE_POINTER_BASE.facts type_pointer_base

create table type_pointer(type TEXT);
.import TYPE_POINTER.facts type_pointer

create table type_struct_field_count(type TEXT, count TEXT);
.import TYPE_STRUCT_FIELD_COUNT.facts type_struct_field_count

create table type_struct_field_name_by_offset(type TEXT, offset TEXT, name TEXT);
.import TYPE_STRUCT_FIELD_NAME_BY_OFFSET.facts type_struct_field_name_by_offset

create table type_struct_field_name(type TEXT, idx TEXT, name TEXT);
.import TYPE_STRUCT_FIELD_NAME.facts type_struct_field_name

create table type_struct_field(type TEXT, idx TEXT, field_type TEXT);
.import TYPE_STRUCT_FIELD.facts type_struct_field

create table type_struct_offset_n(type TEXT, idx TEXT, offset TEXT);
.import TYPE_STRUCT_OFFSET_N.facts type_struct_offset_n

create table type_struct_offset(type TEXT, idx TEXT, offset TEXT);
.import TYPE_STRUCT_OFFSET.facts type_struct_offset

create table type_struct(type TEXT);
.import TYPE_STRUCT.facts type_struct

create table type_union_field_count(type TEXT, count TEXT);
.import TYPE_UNION_FIELD_COUNT.facts type_union_field_count

create table type_union_field_name_by_offset(type TEXT, offset TEXT, name TEXT);
.import TYPE_UNION_FIELD_NAME_BY_OFFSET.facts type_union_field_name_by_offset

create table type_union_field_name(type TEXT, idx TEXT, name TEXT);
.import TYPE_UNION_FIELD_NAME.facts type_union_field_name

create table type_union_field(type TEXT, idx TEXT, field_type TEXT);
.import TYPE_UNION_FIELD.facts type_union_field

create table type_union_offset_n(type TEXT, idx TEXT, offset TEXT);
.import TYPE_UNION_OFFSET_N.facts type_union_offset_n

create table type_union_offset(type TEXT, idx TEXT, offset TEXT);
.import TYPE_UNION_OFFSET.facts type_union_offset

create table type_union(type TEXT);
.import TYPE_UNION.facts type_union

create table vnode_address(vnode TEXT, address TEXT);
.import VNODE_ADDRESS.facts vnode_address

create table vnode_def(vnode TEXT, pcode TEXT);
.import VNODE_DEF.facts vnode_def

create table vnode_desc(vnode TEXT, desc TEXT);
.import VNODE_DESC.facts vnode_desc

create table vnode_hfunc(vnode TEXT, hfunc TEXT);
.import VNODE_HFUNC.facts vnode_hfunc

create table vnode_hvar(vnode TEXT, hvar TEXT);
.import VNODE_HVAR.facts vnode_hvar

create table vnode_is_address(vnode TEXT);
.import VNODE_IS_ADDRESS.facts vnode_is_address

create table vnode_is_addrtied(vnode TEXT);
.import VNODE_IS_ADDRTIED.facts vnode_is_addrtied

create table vnode_name(vnode TEXT, name TEXT);
.import VNODE_NAME.facts vnode_name

create table vnode_offset_n(vnode TEXT, offset TEXT);
.import VNODE_OFFSET_N.facts vnode_offset_n

create table vnode_offset(vnode TEXT, offset TEXT);
.import VNODE_OFFSET.facts vnode_offset

create table vnode_pc_address(vnode TEXT, address TEXT);
.import VNODE_PC_ADDRESS.facts vnode_pc_address

create table vnode_size(vnode TEXT, size TEXT);
.import VNODE_SIZE.facts vnode_size

create table vnode_space(vnode TEXT, space TEXT);
.import VNODE_SPACE.facts vnode_space

create table vnode_tostr(vnode TEXT, tostr TEXT);
.import VNODE_TOSTR.facts vnode_tostr

create table vtable(id TEXT);
.import VTABLE.facts vtable

create table space_offset(space TEXT, offset INTEGER);
.import SPACE_OFFSET.facts space_offset

