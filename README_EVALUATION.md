# Evaluation Framework for ctadl

This framework is designed to evaluate the performance and accuracy of `ctadl` on binary benchmarks, specifically targeting the benchmarks mentioned in the "Operation Mango" (ASCENT) paper (USENIX Security 2024).

## Benchmarks

The "Operation Mango" paper primarily relies on real-world firmware datasets rather than artificial test suites like Juliet. You can use the following datasets for evaluation:

1.  **Karonte Dataset (49 Firmware Samples):**
    *   This is the primary dataset used for comparative evaluation in the paper.
    *   **Download:** The dataset is hosted on Google Drive. You can download it via the link provided in the original Karonte repository: [Download Karonte Dataset](https://drive.google.com/file/d/1-VOf-tEpu4LIgyDyZr7bBZCDK-K2DHaj/view?usp=sharing)
    *   **Note:** This dataset contains full firmware files. You will need to extract the file system (using tools like `binwalk`) and locate the user-space ELF binaries (e.g., `httpd`, `dlnad`) to run through `ctadl`.

2.  **SaTC & Greenhouse Datasets:**
    *   The paper also mentions a handpicked dataset from SaTC and a large-scale "Greenhouse" dataset. These are typically available via the authors' artifact releases or Docker containers on their GitHub: [sefcom/operation-mango-public](https://github.com/sefcom/operation-mango-public).

## Usage

### Prerequisites

- `ctadl` binary built.
- Ghidra installed and `GHIDRA_HOME` environment variable set (for `-l pcode` support).
- Python 3.
- `binwalk` (optional, for extracting firmware images).

### Setting up the Dataset

We provide an automated script to download the Karonte dataset from Google Drive, extract the firmware using `binwalk`, and gather all the user-space ELF binaries into a single directory for easy evaluation.

1.  Ensure you have `binwalk` installed on your system (`sudo apt install binwalk` or `brew install binwalk`).
2.  Run the setup script:
    ```bash
    python3 scripts/setup_karonte_dataset.py
    ```

This script will automatically install `gdown` (if missing) to handle the Google Drive download, extract the images, and place the discovered ELF binaries into `benchmarks/karonte_elfs/`.

### Running the Evaluation

Once the dataset is set up, run the evaluation script:

```bash
python3 scripts/evaluate_benchmarks.py benchmarks/karonte_elfs/ --model benchmarks/firmware_model.json
```

*(Note: The `firmware_model.json` provides a good baseline for common C/C++ sinks like `system()` or `strcpy()`, which are identical to the sinks targeted in the Mango paper for CWE-78 and CWE-121. You may want to extend it to include firmware-specific sources like `nvram_get`).*

### Expected Results

The provided evaluation script (`evaluate_benchmarks.py`) was originally structured for standard benchmark suites with `_bad` / `_good` naming conventions. When running on real firmware binaries:
- The script defaults to expecting a flow (vulnerability) to be found.
- If you are evaluating a known vulnerable binary, a "Passed" result means `ctadl` successfully found a path from a defined source to a sink.

### Reporting

The script provides a summary in the terminal and generates a detailed `evaluation_report.json` containing:
- Status (Passed, Failed, Crashed, Error)
- Phase where it failed (if applicable)
- Number of flows found
- Time taken for each binary

Crashes are reported separately in the summary and detailed in the JSON report with the stderr output.

## Adding New Benchmarks

To add more benchmarks, update `benchmarks/juliet_model.json` with relevant sources and sinks, and ensure your binaries follow the `_bad`/`_good` naming convention or modify the script to support your own convention.
