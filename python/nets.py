import json
import math
import os
import numpy as np
from models import CONFIG, Dataset


class NetTrainer:
    def __init__(self, D, feats, job):
        import torch
        self.torch, self.D, self.feats, self.job = torch, D, feats, job
        self.ci = [D.col[n] for n in feats]
        self.pcols = [D.col[n] for n in ("water.ewm5", "cpu_pkg_w.ewm5", "gpu_w.ewm5", "pump_pow", "fan_duty", "amb_est")]
        self.hs = np.array([int(n.split("_")[1]) for n in D.tnames[:4]], dtype=np.float32)

    def build(self, seed):
        torch, nn = self.torch, self.torch.nn
        torch.manual_seed(seed)
        K, H, O = len(self.feats), self.job.get("hidden", 96), len(self.D.tnames)
        job = self.job
        hs = torch.tensor(self.hs)

        class Net(nn.Module):
            def __init__(m):
                super().__init__()
                m.mlp = nn.Sequential(nn.Linear(K, H), nn.SiLU(), nn.Dropout(job.get("drop", 0.2)), nn.Linear(H, H), nn.SiLU(), nn.Dropout(job.get("drop", 0.2)), nn.Linear(H, O))
                nn.init.zeros_(m.mlp[-1].weight)
                nn.init.zeros_(m.mlp[-1].bias)
                p0 = job["phys_init"]
                m.phys = nn.Parameter(torch.tensor([math.log(p0[0]), math.log(p0[1]), p0[2], p0[3], p0[4], p0[5]], dtype=torch.float32))

            def physics(m, pin):
                C, G, Ta, a, b, q0 = torch.exp(m.phys[0]), torch.exp(m.phys[1]), m.phys[2], m.phys[3], m.phys[4], m.phys[5]
                Tinf = Ta + (a * pin[:, 1] + b * pin[:, 2] + pin[:, 3] + q0) / G
                return (Tinf - pin[:, 0])[:, None] * (1 - torch.exp(-hs[None, :] * G / C))

            def forward(m, x, pin, ys):
                out = m.mlp(x)
                if job.get("phys", True):
                    out = torch.cat([out[:, :4] + m.physics(pin) / ys[:4][None, :], out[:, 4:]], 1)
                return out

        return Net()

    def fit(self, seeds=(0, 1, 2)):
        torch, D, job = self.torch, self.D, self.job
        tr = np.where(D.block == D.bnames.index("train"))[0]
        va = np.where(D.block == D.bnames.index("val"))[0]
        Xtr = D.F[tr][:, self.ci].astype(np.float64)
        mu, sd = np.nanmean(Xtr, 0), np.nanstd(Xtr, 0) + 1e-6
        self.mu, self.sd = mu, sd
        ys = np.nanstd(D.Y[tr], 0).astype(np.float32)
        self.ys = ys
        P = D.F[:, self.pcols]
        lcols = set(D.lhm_cols()) if job.get("lhm_aug") else set()
        li = [i for i, n in enumerate(self.feats) if n in lcols]
        mi = [i for i, n in enumerate(self.feats) if n.startswith("mask_lhm")]

        def prep(idx, aug=0.0, rng=None):
            X = D.F[idx][:, self.ci].astype(np.float64)
            Y = D.Y[idx]
            pin = np.nan_to_num(P[idx].astype(np.float32), nan=0.0)
            pin[:, 4] = pin[:, 4] / 100.0
            if aug:
                pick = rng.random(len(idx)) < aug
                Xa = X[pick].copy()
                Xa[:, li] = np.nan
                Xa[:, mi] = 0.0
                X, Y, pin = np.vstack([X, Xa]), np.vstack([Y, Y[pick]]), np.vstack([pin, pin[pick]])
            Z = np.nan_to_num(np.clip((X - mu) / sd, -8, 8), nan=0.0)
            return torch.tensor(Z, dtype=torch.float32), torch.tensor(pin), torch.tensor(np.nan_to_num(Y / ys)), torch.tensor(np.isfinite(Y))

        ysT = torch.tensor(ys)
        self.models = []
        self.prep = prep
        huber = torch.nn.functional.huber_loss
        for seed in seeds:
            rng = np.random.default_rng(seed)
            xt, pt, yt, mt = prep(tr, job.get("lhm_aug", 0.0), rng)
            xv, pv, yv, mv = prep(va)
            net = self.build(seed)
            opt = torch.optim.AdamW(net.parameters(), lr=job.get("lr", 2e-3), weight_decay=job.get("wd", 1e-2))
            best, bad, state = 1e9, 0, None
            for ep in range(job.get("epochs", 150)):
                net.train()
                perm = torch.randperm(len(xt))
                for i in range(0, len(perm), 256):
                    b = perm[i: i + 256]
                    loss = (huber(net(xt[b], pt[b], ysT), yt[b], reduction="none", delta=1.0) * mt[b]).sum() / mt[b].sum()
                    opt.zero_grad()
                    loss.backward()
                    opt.step()
                net.eval()
                with torch.no_grad():
                    vl = float((huber(net(xv, pv, ysT), yv, reduction="none", delta=1.0) * mv).sum() / mv.sum())
                if vl < best - 1e-5:
                    best, bad, state = vl, 0, {k: v.clone() for k, v in net.state_dict().items()}
                else:
                    bad += 1
                    if bad >= job.get("patience", 15):
                        break
            net.load_state_dict(state)
            net.eval()
            self.models.append(net)
        return self

    def predict_all(self, idx):
        x, p, _, _ = self.prep(idx)
        ys = self.torch.tensor(self.ys)
        with self.torch.no_grad():
            out = np.mean([m(x, p, ys).numpy() for m in self.models], 0)
        return out * self.ys


def nn_task(job):
    import torch
    torch.set_num_threads(2)
    D = Dataset(CONFIG).load()
    tr = NetTrainer(D, job["feats"], job).fit(tuple(job.get("seeds", (0, 1, 2))))
    res = {tg: {"preds": {}} for tg in D.tnames}
    for b in D.bnames:
        idx = np.where(D.block == D.bnames.index(b))[0]
        out = tr.predict_all(idx)
        for j, tg in enumerate(D.tnames):
            res[tg]["preds"][b] = out[np.searchsorted(idx, D.sel(b, tg)), j]
    if job.get("save"):
        w = {"feats": job["feats"], "mu": tr.mu.tolist(), "sd": tr.sd.tolist(), "ys": tr.ys.tolist(), "tnames": D.tnames, "phys": job.get("phys", True),
             "hs": tr.hs.tolist(), "models": [{k: v.numpy().tolist() for k, v in m.state_dict().items()} for m in tr.models]}
        with open(os.path.join(CONFIG["out_dir"], f"{job['tag']}.json"), "w") as f:
            json.dump(w, f)
    return {"tag": job["tag"], "res": res}
