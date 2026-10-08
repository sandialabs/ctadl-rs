// use pest::Parser;
// use pest_derive::Parser;

// #[derive(pest_derive::Parser)]
// #[grammar = "csv.pest"]
// pub struct CSVParser;

#[derive(pest_derive::Parser)]
#[grammar = "flowy.pest"]
pub struct FlowyParser;
