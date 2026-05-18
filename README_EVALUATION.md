# Evaluation Framework for ctadl

This framework is designed to evaluate the performance and accuracy of `ctadl` on binary benchmarks, specifically targeting the benchmarks mentioned in the "Operation Mango" (ASCENT) paper (USENIX Security 2024).

## Benchmarks

The primary benchmarks supported are:
1.  **Juliet Test Suite (C/C++):** Specifically CWE-78 (Command Injection) and CWE-121 (Stack-based Buffer Overflow).
2.  **Real-world Binaries:** Any ELF binaries imported through the Pcode frontend.

## Components

1.  **`benchmarks/juliet_model.json`**: A `ctadl` model file defining common sources and sinks for Juliet test cases.
2.  **`scripts/evaluate_benchmarks.py`**: A Python script that automates the evaluation process.

## Usage

### Prerequisites

- `ctadl` binary built (the script will attempt to build it if missing).
- Ghidra installed and `GHIDRA_HOME` environment variable set (for `-l pcode` support).
- Python 3.

### Running the Evaluation

To run the evaluation on a directory of binaries:

```bash
python3 scripts/evaluate_benchmarks.py /path/to/binaries/ --model benchmarks/juliet_model.json
```

### Expected Results

The script determines expected results based on the filename:
- If the filename contains `_bad`, it expects at least one flow.
- If the filename contains `_good`, it expects zero flows.

### Reporting

The script provides a summary in the terminal and generates a detailed `evaluation_report.json` containing:
- Status (Passed, Failed, Crashed, Error)
- Phase where it failed (if applicable)
- Number of flows found
- Time taken for each binary

Crashes are reported separately in the summary and detailed in the JSON report with the stderr output.

## Adding New Benchmarks

To add more benchmarks, update `benchmarks/juliet_model.json` with relevant sources and sinks, and ensure your binaries follow the `_bad`/`_good` naming convention or modify the script to support your own convention.
