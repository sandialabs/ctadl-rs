import subprocess
import json
import os
import sys
import argparse
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

def run_command(cmd, cwd=None):
    try:
        #print(cmd)
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

def parse_sarif(sarif_path, debug=False):
    try:
        with open(sarif_path, 'r') as f:
            sarif = json.load(f)
        
        flows = []
        sources_matched = 0
        sinks_matched = 0
        
        for run in sarif.get("runs", []):
            for result in run.get("results", []):
                rule_id = result.get("ruleId", "").lower()
                
                # In the human profile, a true complete path is C0001.tainted-path.
                if "c0001" in rule_id:
                    if "codeFlows" in result:
                        flows.append(result)
                
                if debug:
                    if "source" in rule_id:
                        sources_matched += 1
                    elif "sink" in rule_id:
                        sinks_matched += 1
                        
        if debug:
            return flows, sources_matched, sinks_matched
        return flows
    except Exception as e:
        print(f"Error parsing SARIF {sarif_path}: {e}")
        if debug:
            return None, 0, 0
        return None

def evaluate_binary(binary_path, ctadl, model, debug):
    name = os.path.basename(binary_path)
    
    # Determine expectation
    expected_flow = True
    if "_good" in name.lower():
        expected_flow = False
    elif "_bad" in name.lower():
        expected_flow = True
    else:
        # Default to expecting flow if not specified
        expected_flow = True

    start_time = time.time()
    
    # Clean up previous state if any
    run_command([ctadl, "inspect", name])

    # 1. Import
    res = run_command([ctadl, "import", binary_path, "-l", "pcode"])
    if res.returncode != 0:
        return {"name": name, "status": "Crashed", "phase": "import", "error": res.stderr}, "crashed", f"Processing {name} (Expected Flow: {expected_flow})... Import FAILED"

    # 2. Index
    res = run_command([ctadl, "index", name])
    if res.returncode != 0:
        return {"name": name, "status": "Crashed", "phase": "index", "error": res.stderr}, "crashed", f"Processing {name} (Expected Flow: {expected_flow})... Index FAILED"

    # 3. Query
    res = run_command([ctadl, "query", name, "-m", model])
    if res.returncode != 0:
        return {"name": name, "status": "Crashed", "phase": "query", "error": res.stderr}, "crashed", f"Processing {name} (Expected Flow: {expected_flow})... Query FAILED"

    # 4. Format
    sarif_path = f"{name}_results.sarif"
    debug_sarif_path = f"{name}_debug.sarif"
    
    # Always run the human profile to get actual completed flows (C0001.tainted-path)
    res = run_command([ctadl, "format", name, "-o", sarif_path, "--sarif-profile", "human"])
    if res.returncode != 0:
        return {"name": name, "status": "Crashed", "phase": "format", "error": res.stderr}, "crashed", f"Processing {name} (Expected Flow: {expected_flow})... Format FAILED"
        
    sources_matched = 0
    sinks_matched = 0
    if debug:
        # Run the debug profile to check if sources/sinks were matched
        run_command([ctadl, "format", name, "-o", debug_sarif_path, "--sarif-profile", "debug"])
        _, sources_matched, sinks_matched = parse_sarif(debug_sarif_path, debug=True)
        if os.path.exists(debug_sarif_path):
            os.remove(debug_sarif_path)

    # 5. Analyze results
    flows = parse_sarif(sarif_path, debug=False)
        
    duration = time.time() - start_time
    
    # Clean up SARIF
    if os.path.exists(sarif_path):
        os.remove(sarif_path)

    if flows is None:
        return {"name": name, "status": "Error", "phase": "parse", "duration": duration}, "errors", f"Processing {name} (Expected Flow: {expected_flow})... Parse ERROR"
    else:
        found_flow = len(flows) > 0
        passed = (found_flow == expected_flow)
        
        status = "Passed" if passed else "Failed"
        summary_key = "passed" if passed else "failed"
        
        log_msg = f"Processing {name} (Expected Flow: {expected_flow})... {status.upper()} ({len(flows)} flows found in {duration:.2f}s)"
        if debug:
            if sources_matched == 0 or sinks_matched == 0:
                log_msg += f"\n    [!] Debug Warning: matched {sources_matched} sources and {sinks_matched} sinks. Need >0 of both to find flows."
        
        result_data = {
            "name": name,
            "status": status,
            "expected_flow": expected_flow,
            "found_flow": found_flow,
            "num_flows": len(flows),
            "duration": duration
        }
        if debug:
            result_data["sources_matched"] = sources_matched
            result_data["sinks_matched"] = sinks_matched
            
        return result_data, summary_key, log_msg

def main():
    parser = argparse.ArgumentParser(description="Evaluate ctadl on benchmarks.")
    parser.add_argument("binary_dir", help="Directory containing benchmark binaries")
    parser.add_argument("--model", default="benchmarks/firmware_model.json", help="Path to the query model JSON")
    parser.add_argument("--output", default="evaluation_report.json", help="Path to save the evaluation report")
    parser.add_argument("--ctadl", default="target/release/ctadl", help="Path to the ctadl binary")
    parser.add_argument("--debug", action="store_true", help="Enable debug SARIF profile and check for source/sink matches")
    parser.add_argument("-j", "--jobs", type=int, default=1, help="Number of parallel jobs")
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

    with ThreadPoolExecutor(max_workers=args.jobs) as executor:
        futures = {executor.submit(evaluate_binary, os.path.join(args.binary_dir, b), args.ctadl, args.model, args.debug): b for b in binaries}
        
        for future in as_completed(futures):
            result_data, summary_key, log_msg = future.result()
            print(log_msg)
            report["results"].append(result_data)
            report["summary"][summary_key] += 1

    # Sort results by name to maintain a consistent report
    report["results"].sort(key=lambda x: x["name"])

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
