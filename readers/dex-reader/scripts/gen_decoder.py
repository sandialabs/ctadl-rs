import re

formats = ["" for _ in range(256)]

with open("formats.txt") as fmt:
    for line in fmt.readlines():
        line = line.strip()
        [format, ops] = line.split(':')
        ops = ops.strip().split(' | ')
        codes = []
        for op in ops:
            rng = op.split('..=')
            if len(rng) == 2:
                start = int(rng[0][2:], 16)
                end = int(rng[1][2:], 16)
                codes.extend(range(start, end+1))
            else:
                assert len(rng) == 1
                codes.append(int(op[2:], 16))
        for code in codes:
            formats[code] = format

opcodes = []

with open("opcodes.txt") as opin:
    for opcode in opin.readlines():
        opcodes.append(opcode.strip())

assert len(opcodes) == len(formats)

param=re.compile("<(.).*>")
def format_name(fmt: str):
    return param.sub(r"\1", fmt)


print("#[repr(u8)]")
print("pub enum Instruction {")
for i in range(256):
    print(f"    {opcodes[i]}(Format{formats[i]}) = 0x{i:02x},")
print("}")

print("impl Instruction {")
print("    pub fn new(insns: &[u16]) -> (usize, Self) {")
print("        match (insns[0] & 0xFF) as u8 {")
for i in range(256):
    fmt=formats[i].replace('<', '::<')
    print(f"        0x{i:x} => {{ let (size, fmt) = Format{fmt}::new(insns); (size, Instruction::{opcodes[i]}(fmt)) }}")
print("        }")
print("    }")
print("    pub fn format(&self) -> Format<'_> {")
print("        match self {")
for i in range(256):
    print(f"            Self::{opcodes[i]}(fmt) => Format::Format{format_name(formats[i])}(fmt),")
print("        }")
print("    }")
print("}")

print("pub enum Format<'a> {")
for format in sorted(set(formats)):
    print(f"    Format{format_name(format)}(&'a Format{format}),")
print("}")
