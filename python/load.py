import numpy as np
from lake import Lake

CONFIG = {
    "table": "telemetry",
    "step": 1,
    "roles": {"X": "state", "U": "input", "D": "disturbance"},
}


class Loader:
    def __init__(self, config=CONFIG, lake=None):
        self.cfg = config
        self.lake = lake or Lake()
        self.con = self.lake.con

    def rows(self, t0=None, t1=None, device=None):
        step = self.cfg["step"]
        where, args = ["role in (select unnest(?))"], [list(self.cfg["roles"].values())]
        if t0 is not None:
            where.append("epoch(ts) >= ?")
            args.append(float(t0))
        if t1 is not None:
            where.append("epoch(ts) < ?")
            args.append(float(t1))
        if device is not None:
            where.append("source = ?")
            args.append(device)
        sql = (
            f"select cast(floor(epoch(ts) / {step}) as bigint) as k, role, channel as name,"
            f"arg_max(value, ts) as v from {self.cfg['table']} where {' and '.join(where)} group by all order by k"
        )
        return self.con.execute(sql, args).fetchnumpy()

    @staticmethod
    def ffill(m):
        valid = ~np.isnan(m)
        idx = np.where(valid, np.arange(m.shape[0])[:, None], -1)
        np.maximum.accumulate(idx, axis=0, out=idx)
        out = np.take_along_axis(m, np.maximum(idx, 0), axis=0)
        out[idx < 0] = np.nan
        return out

    def pivot(self, data):
        import torch
        k, role, name, v = data["k"], data["role"], data["name"], data["v"]
        if len(k) == 0:
            empty = torch.zeros((0, 0), dtype=torch.float32)
            return {"t": torch.zeros(0, dtype=torch.float64), "names": {n: [] for n in self.cfg["roles"]}, **{n: empty for n in self.cfg["roles"]}}
        k0, k1 = int(k.min()), int(k.max())
        out = {"t": torch.from_numpy((np.arange(k0, k1 + 1) * self.cfg["step"]).astype(np.float64)), "names": {}}
        for key, r in self.cfg["roles"].items():
            sel = role == r
            names = sorted(set(name[sel]))
            col = {n: i for i, n in enumerate(names)}
            m = np.full((k1 - k0 + 1, len(names)), np.nan, dtype=np.float32)
            if names:
                m[k[sel] - k0, [col[n] for n in name[sel]]] = v[sel]
                m = self.ffill(m)
            out[key] = torch.from_numpy(m)
            out["names"][key] = names
        return out

    def load(self, t0=None, t1=None, device=None):
        return self.pivot(self.rows(t0, t1, device))


if __name__ == "__main__":
    d = Loader().load()
    print({k: tuple(d[k].shape) for k in ("t", "X", "U", "D")})
