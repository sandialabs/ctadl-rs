use std::fs;
use std::io::Read;
use std::path::Path;

use dex_reader::APKParser;
use dex_reader::parser::*;

#[test]
fn test_dex_files_do_not_crash() {
    let dex_files_dir = Path::new("dex-files");

    // Check if the directory exists
    if !dex_files_dir.exists() {
        panic!("dex-files directory not found");
    }

    // Get all files in the directory
    let entries = fs::read_dir(dex_files_dir).expect("Failed to read dex-files directory");

    let mut files_found = 0;
    for entry in entries {
        let entry = entry.expect("Failed to read directory entry");
        let path = entry.path();
        if path.is_file() {
            let ext = path.extension().and_then(|s| s.to_str());
            match ext {
                Some("dex") => {
                    files_found += 1;
                    println!("Testing DEX file: {}", path.display());
                    let mut file = fs::File::open(&path).expect("Failed to open dex file");
                    let mut buffer = Vec::new();
                    file.read_to_end(&mut buffer)
                        .expect("Failed to read dex file");
                    test_dex_buffer(path.to_str().unwrap(), &buffer);
                }
                Some("apk") => {
                    files_found += 1;
                    println!("Testing APK file: {}", path.display());
                    let mut file = fs::File::open(&path).expect("Failed to open apk file");
                    let mut buffer = Vec::new();
                    file.read_to_end(&mut buffer)
                        .expect("Failed to read apk file");
                    let apk = APKParser::new(&buffer).expect("Failed to parse APK");
                    for (name, parser) in apk.dex_parsers_with_filenames() {
                        let full_name = format!("{}:{}", path.display(), name);
                        println!("Testing DEX entry {} from APK", name);
                        test_dex_buffer(&full_name, parser.data);
                    }
                }
                _ => {}
            }
        }
    }

    if files_found == 0 {
        panic!("No .dex or .apk files found in dex-files directory");
    }
}

fn test_dex_buffer(dex_file: &str, buffer: &[u8]) {
    // Parse the DEX file - this exercises all parsing logic including catch handlers
    let header =
        parse_dex_header(buffer).expect(&format!("Failed to parse header for: {}", dex_file));
    let map = parse_map_list(buffer, &header)
        .expect(&format!("Failed to parse map list for: {}", dex_file));
    validate_map_against_header(&map, &header)
        .expect(&format!("Failed to validate map for: {}", dex_file));

    let _strings = parse_string_ids(buffer, &header)
        .expect(&format!("Failed to parse string IDs for: {}", dex_file));
    let _type_ids = parse_type_ids(buffer, &header)
        .expect(&format!("Failed to parse type IDs for: {}", dex_file));
    let _proto_ids = parse_proto_ids(buffer, &header)
        .expect(&format!("Failed to parse proto IDs for: {}", dex_file));
    let class_defs = parse_class_defs(buffer, &header)
        .expect(&format!("Failed to parse class defs for: {}", dex_file));
    let _methods = parse_method_ids(buffer, &header)
        .expect(&format!("Failed to parse method IDs for: {}", dex_file));
    let _field_ids = parse_field_ids(buffer, header.field_ids_off, header.field_ids_size)
        .expect(&format!("Failed to parse field IDs for: {}", dex_file));

    // Parse class data and code items to exercise catch handler parsing
    for class_def in &class_defs {
        let class_data = class_def
            .parse_class_data(buffer)
            .expect(&format!("Failed to parse class data for: {}", dex_file));

        // Parse code items which includes catch handlers
        for method in class_data
            .direct_methods
            .iter()
            .chain(class_data.virtual_methods.iter())
        {
            if let Some(code_item) = method
                .code(buffer)
                .expect(&format!("Failed to parse code item for: {}", dex_file))
            {
                // Access handlers to ensure they were parsed correctly
                if let Some(ref handlers) = code_item.handlers {
                    // Verify we can look up handlers by offset
                    for try_item in &code_item.tries {
                        let _handler = handlers.get_by_off(try_item.handler_off);
                        // Handler lookup may return None if offset doesn't match, which is OK
                    }
                }
            }
        }
    }

    println!("Successfully parsed: {}", dex_file);
}
