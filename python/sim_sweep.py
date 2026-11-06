import itertools
import os
import subprocess
import sys

import ray

CONFIG = {
    "exe": os.environ["DL_EXE"],
    "out": os.environ["DL_OUT"],
    "cores_per_case": 1,
    "in_flight": int(os.environ.get("DL_IN_FLIGHT", "6")),
    "plants": ["measured", "alternative"],
    "pairs": [(30, 30), (10, 5), (5, 5), (5, 30), (2, 5), (1, 5), (1, 30)],
    "informed_loads": [f"const:{40 + 2.5 * i}" for i in range(17)] + ["const:100", "const:200", "const:300", "const:400", "sq:60:300:600:600"],
    "xgb_loads": ["const:40", "const:50", "const:60", "const:70", "const:80", "const:100", "const:200", "const:400", "sq:60:300:600:600"],
    "step": ["step:60:260:3600", "step:100:300:3600", "step:200:400:3600"],
}


@ray.remote(num_cpus=CONFIG["cores_per_case"])
def run(case):
    env = dict(os.environ, DL_CASE=case, DL_SIM_THREADS="1")
    r = subprocess.run([CONFIG["exe"], "r36_one_case", "--ignored", "--nocapture", "--test-threads=1"], env=env, capture_output=True, text=True)
    lines = [x for x in r.stdout.splitlines() if "R36 " in x]
    return case, (lines[0][lines[0].index("R36 "):] if lines else "FAILED " + r.stderr[-300:])


def cases(which):
    out = []
    for kind in which:
        loads = (CONFIG["informed_loads"] if kind == "informed" else CONFIG["xgb_loads"]) + CONFIG["step"]
        for plant, load, (plan, write) in itertools.product(CONFIG["plants"], loads, CONFIG["pairs"]):
            out.append(f"{kind};{plant};{load};{plan};{write}")
    return out


if __name__ == "__main__":
    ray.init(address="auto", logging_level="ERROR")
    todo = cases(sys.argv[1:] or ["informed", "xgb"])
    todo.sort(key=lambda c: (not c.startswith("xgb"), int(c.split(";")[3])))
    pending, done = [], 0
    with open(CONFIG["out"], "a") as f:
        while todo or pending:
            while todo and len(pending) < CONFIG["in_flight"]:
                pending.append(run.remote(todo.pop(0)))
            ready, pending = ray.wait(pending, num_returns=1)
            for r in ready:
                case, line = ray.get(r)
                f.write(line + "\n")
                f.flush()
                done += 1
    print("done", done)
