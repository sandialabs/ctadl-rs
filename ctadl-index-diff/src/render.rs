//! Printing canonical rows by content. A column that holds no instruction prints as the index
//! dump prints it ([`DumpCol`]); one that does needs the signatures, which [`Cx`] carries.

use ctadl_index::index_engine::dump::{self, DumpCol};

use crate::insn::Signatures;

/// What a canonical row needs to be ordered and printed by content.
pub struct Cx<'a> {
    dump: dump::Cx<'a>,
    signatures: &'a Signatures,
}

impl<'a> Cx<'a> {
    pub fn new(signatures: &'a Signatures) -> Self {
        // A canonical row holds no function id, so no id map is needed.
        Cx {
            dump: dump::Cx::new(None),
            signatures,
        }
    }

    pub fn signatures(&self) -> &'a Signatures {
        self.signatures
    }
}

/// One column of a canonical row.
pub trait Col {
    fn col(&self, cx: &Cx<'_>, out: &mut String);
}

impl<T: DumpCol> Col for T {
    fn col(&self, cx: &Cx<'_>, out: &mut String) {
        self.dump_col(&cx.dump, out)
    }
}

/// A whole canonical row: a tuple of columns, separated by tabs as in the index dump.
pub trait Row {
    fn row(&self, cx: &Cx<'_>, out: &mut String);
}

macro_rules! row_tuple {
    ($first:ident $(, $rest:ident)*) => {
        impl<$first: Col $(, $rest: Col)*> Row for ($first, $($rest,)*) {
            #[allow(non_snake_case)]
            fn row(&self, cx: &Cx<'_>, out: &mut String) {
                let ($first, $($rest,)*) = self;
                dump::push_escaped(out, |out| $first.col(cx, out));
                $(
                    out.push('\t');
                    dump::push_escaped(out, |out| $rest.col(cx, out));
                )*
            }
        }
    };
}
row_tuple!(A);
row_tuple!(A, B);
row_tuple!(A, B, C);
row_tuple!(A, B, C, D);
row_tuple!(A, B, C, D, E);
row_tuple!(A, B, C, D, E, F);
row_tuple!(A, B, C, D, E, F, G);
