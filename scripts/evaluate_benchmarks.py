import subprocess
import json
import os
import sys
import argparse
import time

def run_command(cmd, cwd=None):
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, cwd=cwd)
        return result
    except Exception as e:
        class Dummy:
            pass
        d = Dummy()
        d.returncode = -1
        d.stdout = ""
        d.stderr = str(e)
        return d

def parse_sarif(sarif_path):
    try:
        with open(sarif_path, 'r') as f:
            sarif = json.load(f)
        
        flows = []
        for run in sarif.get("runs", []):
            for result in run.get("results", []):
                if "codeFlows" in result:
                    flows.append(result)
        return flows
    except Exception as e:
        print(f"Error parsing SARIF {sarif_path}: {e}")
        return None

def main():
    parser = argparse.ArgumentParser(description="Evaluate ctadl on benchmarks.")
    parser.add_argument("binary_dir", help="Directory containing benchmark binaries")
    parser.add_argument("--model", default="benchmarks/juliet_model.json", help="Path to the query model JSON")
    parser.add_argument("--output", default="evaluation_report.json", help="Path to save the evaluation report")
    parser.add_argument("--ctadl", default="target/release/ctadl", help="Path to the ctadl binary")
    args = parser.parse_args()

    if not os.path.exists(args.ctadl):
        print(f"ctadl binary not found at {args.ctadl}. Building it now...")
        res = run_command(["cargo", "build", "--release"])
        if res.returncode != 0:
            print("Failed to build ctadl.")
            print(res.stderr)
            sys.exit(1)

    binaries = [f for f in os.listdir(args.binary_dir) if os.path.isfile(os.path.join(args.binary_dir, f))]
    binaries.sort()

    report = {
        "summary": {
            "total": len(binaries),
            "passed": 0,
            "failed": 0,
            "crashed": 0,
            "errors": 0
        },
        "results": []
    }

    print(f"Found {len(binaries)} binaries in {args.binary_dir}")

    for binary in binaries:
        binary_path = os.path.join(args.binary_dir, binary)
        name = binary
        
        # Determine expectation
        expected_flow = True
        if "_good" in name.lower():
            expected_flow = False
        elif "_bad" in name.lower():
            expected_flow = True
        else:
            # Default to expecting flow if not specified
            expected_flow = True

        print(f"Processing {name} (Expected Flow: {expected_flow})... ", end="", flush=True)
        
        start_time = time.time()
        
        # Clean up previous state if any
        run_command([args.ctadl, "inspect", name]) # Just to check if it exists? No, ctadl doesn't have a clear way to delete.
        # Assume it's a fresh run or we don't care about persistence for now.

        # 1. Import
        res = run_command([args.ctadl, "import", binary_path, "-l", "pcode"])
        if res.returncode != 0:
            print("Import FAILED")
            report["results"].append({"name": name, "status": "Crashed", "phase": "import", "error": res.stderr})
            report["summary"]["crashed"] += 1
            continue

        # 2. Index
        res = run_command([args.ctadl, "index", name])
        if res.returncode != 0:
            print("Index FAILED")
            report["results"].append({"name": name, "status": "Crashed", "phase": "index", "error": res.stderr})
            report["summary"]["crashed"] += 1
            continue

        # 3. Query
        res = run_command([args.ctadl, "query", name, "-m", args.model])
        if res.returncode != 0:
            print("Query FAILED")
            report["results"].append({"name": name, "status": "Crashed", "phase": "query", "error": res.stderr})
            report["summary"]["crashed"] += 1
            continue

        # 4. Format
        sarif_path = f"{name}_results.sarif"
        res = run_command([args.ctadl, "format", name, "-o", sarif_path])
        if res.returncode != 0:
            print("Format FAILED")
            report["results"].append({"name": name, "status": "Crashed", "phase": "format", "error": res.stderr})
            report["summary"]["crashed"] += 1
            continue

        # 5. Analyze results
        flows = parse_sarif(sarif_path)
        duration = time.time() - start_time
        
        if flows is None:
            print("Parse ERROR")
            report["results"].append({"name": name, "status": "Error", "phase": "parse", "duration": duration})
            report["summary"]["errors"] += 1
        else:
            found_flow = len(flows) > 0
            passed = (found_flow == expected_flow)
            
            if passed:
                print(f"PASSED ({len(flows)} flows found in {duration:.2f}s)")
                report["summary"]["passed"] += 1
                status = "Passed"
            else:
                print(f"FAILED ({len(flows)} flows found, expected {expected_flow} in {duration:.2f}s)")
                report["summary"]["failed"] += 1
                status = "Failed"
            
            report["results"].append({
                "name": name,
                "status": status,
                "expected_flow": expected_flow,
                "found_flow": found_flow,
                "num_flows": len(flows),
                "duration": duration
            })
        
        # Clean up SARIF
        if os.path.exists(sarif_path):
            os.remove(sarif_path)

    # Final summary
    print("\n" + "="*40)
    print("Evaluation Summary")
    print("="*40)
    for k, v in report["summary"].items():
        print(f"{k.capitalize()}: {v}")
    print("="*40)

    with open(args.output, 'w') as f:
        json.dump(report, f, indent=2)
    print(f"Detailed report saved to {args.output}")

if __name__ == "__main__":
    main()
