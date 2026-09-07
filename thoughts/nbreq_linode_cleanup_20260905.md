# Linode nbreq cleanup — 5 September 2026

Completed the authorized cleanup of clearly regenerable material older than **2 September 2026, 00:00 NZST (+12:00)**. Both file modification and metadata-change dates were checked for every entry; directory dates alone were not used.

Removed **38 Cargo build/cache directories**. Observed free-space increase: **24.20 GiB**, from 3.24 GiB to **27.44 GiB**. `df` rounded the final available space to 28G (41% used).

Every deleted target had Cargo build markers, no source manifest at its root, no Git metadata or tracked files, no symlinks or nested mounts, and no current-user process references to its owning work area. Exact paths and tree identities were checked again immediately before deletion. Rebuilding the retained source/lockfiles regenerates these outputs; the audit-tool build cache is also regenerable.

September 2 work and the September 5 M0 labs were retained. Post-cleanup inspection verified the previously observed newer roots unchanged. All 116 current M0 source hashes still pass; verifier/warmup exit markers remain zero and both F5 binaries remain executable. No application source was changed.

## Retained for review

Nothing in the following groups has been deleted except its separately listed Cargo build directories. Sizes below describe the remaining material, after cleanup. Age is the original newest metadata-change date before parent-directory dates changed during cleanup.

| Group | Material | Count | Remaining size | Recommendation |
| --- | --- | ---: | ---: | --- |
| R1 | Old Git checkout with uncommitted work | 1 | 1.35 MiB | Keep: contains local edits and an untracked experiment. |
| R2 | Source snapshots and test work areas | 38 | 117.38 MiB | Review before removal: snapshots may contain changes or fixtures absent from committed history. |
| R3 | Packaged GDS/Wine test binaries and proof bundles | 11 | 271.38 MiB | Review before removal: these are packaged deliverables and platform proof material, not Cargo caches. |
| R4 | Source/package archives and Git bundle | 64 | 104.15 MiB | Review before removal: an archive may be the only preserved copy of its exact candidate. |
| R5 | Historical logs, scripts, process IDs and result markers | 100 | 9.13 MiB | Review before removal: logs may be the original failure/verification evidence; launch scripts may document reproduction. |

The old checkout `/home/ubuntu/nbreq` is at `2ddcbb3ff2df336c7f18816d5507f96c66a629ff` and has:

- Modified `src/backend/curl.rs`.
- Modified `src/curl_tests.rs`.
- Untracked `experiments/linux-curl-resolver/`.

Preserve or archive those changes before considering removal of that checkout. Its generated `target` directory alone was removed.

### R1 — Old Git checkout with uncommitted work

| Remote path | Remaining size | Original latest metadata date |
| --- | ---: | --- |
| `/home/ubuntu/nbreq` | 1.35 MiB | 2026-08-17 |

### R2 — Source snapshots and test work areas

| Remote path | Remaining size | Original latest metadata date |
| --- | ---: | --- |
| `/home/ubuntu/nbreq-native-wp4-6eb5206.Efhgo3` | 1.09 MiB | 2026-08-17 |
| `/home/ubuntu/nbreq-wp11.2-4aaa56e-run2` | 2.50 MiB | 2026-08-24 |
| `/home/ubuntu/nbreq-wp11.2-de21963-run1` | 2.50 MiB | 2026-08-24 |
| `/home/ubuntu/nbreq-wp4-072c9d0` | 1.09 MiB | 2026-08-17 |
| `/home/ubuntu/nbreq-wp4-bcfae5a` | 1.09 MiB | 2026-08-17 |
| `/home/ubuntu/nbreq-wp6-b367247` | 1.21 MiB | 2026-08-20 |
| `/home/ubuntu/nbreq-wp8-3223d8e-QQ42Gy` | 1.48 MiB | 2026-08-20 |
| `/home/ubuntu/nbreq-wp8-735cb9f-0WPIJj` | 1.48 MiB | 2026-08-20 |
| `/home/ubuntu/nbreq-wp8-c1f123e-NlkfxB` | 1.49 MiB | 2026-08-20 |
| `/home/ubuntu/nbreq-wp9-33c43ed-run` | 1.61 MiB | 2026-08-21 |
| `/home/ubuntu/nbreq-wp9-97d3c13-run` | 1.61 MiB | 2026-08-21 |
| `/home/ubuntu/nbreq-wp9.3-bba1d24-run` | 1.64 MiB | 2026-08-21 |
| `/home/ubuntu/nbreq-wp9.4j-proof-d3d2809` | 1.91 MiB | 2026-08-22 |
| `/home/ubuntu/nbreq-wp9.4j-proof-f749df5` | 1.91 MiB | 2026-08-22 |
| `/home/ubuntu/nbreq-wp9.5b-1daedb4-proof` | 1.96 MiB | 2026-08-22 |
| `/home/ubuntu/nbreq-wp9.5c-c7467d5-proof` | 1.96 MiB | 2026-08-22 |
| `/tmp/nbreq-d4-07128d0-r2.UhwTiz` | 2.69 MiB | 2026-08-27 |
| `/tmp/nbreq-d4-07128d0-r3.OZr59d` | 2.66 MiB | 2026-08-27 |
| `/tmp/nbreq-d4-07128d0-r4.YAzTQf` | 2.66 MiB | 2026-08-27 |
| `/tmp/nbreq-d4-07128d0-r5.zaiUWo` | 2.72 MiB | 2026-08-27 |
| `/tmp/nbreq-d4-07128d0.2toj4c` | 2.69 MiB | 2026-08-27 |
| `/tmp/nbreq-f6-final.1tivmk` | 11.27 MiB | 2026-08-30 |
| `/tmp/nbreq-f6-final2.HgpnTr` | 11.27 MiB | 2026-08-30 |
| `/tmp/nbreq-f6-final3.iexL0d` | 11.27 MiB | 2026-08-30 |
| `/tmp/nbreq-f6-final4.HjZEVj` | 3.40 MiB | 2026-08-30 |
| `/tmp/nbreq-f6-final5.QPE5va` | 3.41 MiB | 2026-08-30 |
| `/tmp/nbreq-p10-07-38a4aca.GKyxmY` | 2.43 MiB | 2026-08-23 |
| `/tmp/nbreq-v010-dns-final.4sLKja` | 2.60 MiB | 2026-08-27 |
| `/tmp/nbreq-v010-dns-green.brdztp` | 2.59 MiB | 2026-08-27 |
| `/tmp/nbreq-v010-dns-green2.ohfZqC` | 2.59 MiB | 2026-08-27 |
| `/tmp/nbreq-v010-red.ZsBmgR` | 2.59 MiB | 2026-08-27 |
| `/tmp/nbreq-wp11-4aaa56e.3SrvL3` | 2.50 MiB | 2026-08-24 |
| `/tmp/nbreq-wp7-724bf65.3rqQ4q` | 1.30 MiB | 2026-08-20 |
| `/tmp/nbreq-wp7-81d715e.2SMtRV` | 1.30 MiB | 2026-08-20 |
| `/tmp/nbreq-wp7-cc96305.dNj6zk` | 1.30 MiB | 2026-08-20 |
| `/tmp/nbreq-wp95-compare-vm6LoE` | 2.19 MiB | 2026-08-22 |
| `/tmp/nbreq-wp95-final-95b61a6` | 13.29 MiB | 2026-08-23 |
| `/tmp/nbreq-wp95-pressure-JccxGg` | 2.15 MiB | 2026-08-22 |

### R3 — Packaged GDS/Wine test binaries and proof bundles

| Remote path | Remaining size | Original latest metadata date |
| --- | ---: | --- |
| `/home/ubuntu/gds-nbreq-native-p10-06` | 61.56 MiB | 2026-08-23 |
| `/home/ubuntu/gds-nbreq-native-wine5-6c3bde6` | 40.15 MiB | 2026-08-23 |
| `/home/ubuntu/nbreq-f2-5-wine` | 2.31 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-g4-51269a0` | 39.48 MiB | 2026-08-17 |
| `/home/ubuntu/nbreq-g4-7a1a7e7` | 39.44 MiB | 2026-08-17 |
| `/home/ubuntu/nbreq-g4-96cf352` | 14.52 MiB | 2026-08-17 |
| `/home/ubuntu/nbreq-g5-35902c4` | 40.30 MiB | 2026-08-17 |
| `/home/ubuntu/nbreq-wine-2ddcbb3` | 5.51 MiB | 2026-08-17 |
| `/home/ubuntu/nbreq-wine-msrv-2ddcbb3` | 10.48 MiB | 2026-08-17 |
| `/home/ubuntu/nbreq-wine-wp4-072c9d0.CBf8xG` | 8.81 MiB | 2026-08-17 |
| `/home/ubuntu/nbreq-wine-wp4-6eb5206.3B5rd5` | 8.81 MiB | 2026-08-17 |

### R4 — Source/package archives and Git bundle

| Remote path | Remaining size | Original latest metadata date |
| --- | ---: | --- |
| `/home/ubuntu/gds-nbreq-curl-pilot-g4-51269a0.zip` | 15.17 MiB | 2026-08-17 |
| `/home/ubuntu/gds-nbreq-curl-pilot-g4-7a1a7e7.zip` | 15.16 MiB | 2026-08-17 |
| `/home/ubuntu/gds-nbreq-curl-pilot-g4-96cf352.zip` | 6.02 MiB | 2026-08-17 |
| `/home/ubuntu/gds-nbreq-curl-pilot-g5-35902c4.zip` | 15.17 MiB | 2026-08-17 |
| `/home/ubuntu/gds-nbreq-native-x86.zip` | 15.21 MiB | 2026-08-23 |
| `/home/ubuntu/nbreq-2ddcbb3.bundle` | 0.22 MiB | 2026-08-17 |
| `/home/ubuntu/nbreq-d4-harness-07128d0.tar.gz` | 0.01 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-d5-forwardport-b163266.tar.gz` | 0.56 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-dns-hotfix-07128d0.tar.gz` | 0.50 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-f2-5-final-ecac4c1.tar.gz` | 0.63 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f2-5-windows-green-ecac4c1.tar.gz` | 0.63 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f2.2-windows-1236bde-working.tar.gz` | 0.61 MiB | 2026-08-28 |
| `/home/ubuntu/nbreq-f23-final-a0a6071.tar.gz` | 0.61 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f23-same-batch-a0a6071.tar.gz` | 0.61 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f23-windows-green-a0a6071.tar.gz` | 0.61 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f24-final-windows-checkpoint.tar.gz` | 0.62 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f24-windows-checkpoint.tar.gz` | 0.62 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f3-candidate.tar.gz` | 0.64 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f4-1-final-004FCFAC.tar.gz` | 0.64 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f4-1-final-BEEF28B4.tar.gz` | 0.64 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f4-1-windows-candidate.tar.gz` | 0.64 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f4-2-FF974A54.tar.gz` | 0.64 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f4-3-5881F96B.tar.gz` | 0.64 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-f4-4-eef505ba.tar.gz` | 0.64 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-f6-final-candidate.tar.gz` | 0.79 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-f6-final2-candidate.tar.gz` | 0.79 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-f6-final3-candidate.tar.gz` | 0.79 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-f6-final4-candidate.tar.gz` | 0.66 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-f6-final5-candidate.tar.gz` | 0.66 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-p10-07-38a4aca.zip` | 0.51 MiB | 2026-08-23 |
| `/home/ubuntu/nbreq-p10-07-95cc8ea.zip` | 0.51 MiB | 2026-08-23 |
| `/home/ubuntu/nbreq-v010-dns-final-f08ee60.tar.gz` | 0.50 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-v010-dns-green-f08ee60.tar.gz` | 0.50 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-v010-dns-green2-f08ee60.tar.gz` | 0.50 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-v010-dns-red-f08ee60.tar.gz` | 0.50 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-win10-proof-6eb5206.zip` | 3.22 MiB | 2026-08-17 |
| `/home/ubuntu/nbreq-win10-proof.zip` | 3.22 MiB | 2026-08-17 |
| `/home/ubuntu/nbreq-wine-2ddcbb3.zip` | 1.98 MiB | 2026-08-17 |
| `/home/ubuntu/nbreq-wine-msrv-2ddcbb3.zip` | 2.01 MiB | 2026-08-17 |
| `/home/ubuntu/nbreq-wp11.2-4aaa56e.zip` | 0.54 MiB | 2026-08-24 |
| `/home/ubuntu/nbreq-wp11.2-de21963.zip` | 0.54 MiB | 2026-08-24 |
| `/home/ubuntu/nbreq-wp4-072c9d0.tar.gz` | 0.21 MiB | 2026-08-17 |
| `/home/ubuntu/nbreq-wp4-6eb5206.tar.gz` | 0.21 MiB | 2026-08-17 |
| `/home/ubuntu/nbreq-wp4-bcfae5a.tar.gz` | 0.21 MiB | 2026-08-17 |
| `/home/ubuntu/nbreq-wp6-b367247-source.zip` | 0.28 MiB | 2026-08-20 |
| `/home/ubuntu/nbreq-wp7-724bf65-source.zip` | 0.30 MiB | 2026-08-20 |
| `/home/ubuntu/nbreq-wp7-81d715e-source.zip` | 0.30 MiB | 2026-08-20 |
| `/home/ubuntu/nbreq-wp7-cc96305-source.zip` | 0.30 MiB | 2026-08-20 |
| `/home/ubuntu/nbreq-wp8-3223d8e-source.zip` | 0.34 MiB | 2026-08-20 |
| `/home/ubuntu/nbreq-wp8-735cb9f-source.zip` | 0.34 MiB | 2026-08-20 |
| `/home/ubuntu/nbreq-wp8-c1f123e-source.zip` | 0.34 MiB | 2026-08-20 |
| `/home/ubuntu/nbreq-wp9-33c43ed-source.zip` | 0.36 MiB | 2026-08-21 |
| `/home/ubuntu/nbreq-wp9-97d3c13-source.zip` | 0.36 MiB | 2026-08-21 |
| `/home/ubuntu/nbreq-wp9.3-bba1d24.zip` | 0.36 MiB | 2026-08-21 |
| `/home/ubuntu/nbreq-wp9.4j-d3d2809.zip` | 0.41 MiB | 2026-08-22 |
| `/home/ubuntu/nbreq-wp9.4j-f749df5.zip` | 0.41 MiB | 2026-08-22 |
| `/home/ubuntu/nbreq-wp9.5-alloc-0e05a40.zip` | 0.46 MiB | 2026-08-22 |
| `/home/ubuntu/nbreq-wp9.5-compare-4453bf2.zip` | 0.45 MiB | 2026-08-22 |
| `/home/ubuntu/nbreq-wp9.5-final-95b61a6.zip` | 0.46 MiB | 2026-08-22 |
| `/home/ubuntu/nbreq-wp9.5-pressure-234e07b.zip` | 0.45 MiB | 2026-08-22 |
| `/home/ubuntu/nbreq-wp9.5b-1daedb4.zip` | 0.42 MiB | 2026-08-22 |
| `/home/ubuntu/nbreq-wp9.5c-c7467d5.zip` | 0.42 MiB | 2026-08-22 |
| `/tmp/nbreq-f1-http-view-working-tree.tar.gz` | 0.56 MiB | 2026-08-25 |
| `/tmp/nbreq-f1-r3-working-tree.tar.gz` | 0.56 MiB | 2026-08-25 |

### R5 — Historical logs, scripts, process IDs and result markers

| Remote path | Remaining size | Original latest metadata date |
| --- | ---: | --- |
| `/home/ubuntu/launch-nbreq-f4-2-ubuntu.sh` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/launch-nbreq-f4-3-ubuntu.sh` | 0.00 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-d4-07128d0-r2.log` | 0.00 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-d4-07128d0-r2.state` | 0.00 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-d4-07128d0-r3.log` | 0.00 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-d4-07128d0-r3.state` | 0.00 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-d4-07128d0-r4.log` | 0.00 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-d4-07128d0-r4.state` | 0.00 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-d4-07128d0-r5-verify2.log` | 0.09 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-d4-07128d0-r5-verify2.state` | 0.00 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-d4-07128d0-r5.log` | 0.01 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-d4-07128d0-r5.state` | 0.00 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-d4-07128d0.log` | 0.00 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-d4-07128d0.state` | 0.00 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-d5-verify.sh` | 0.00 MiB | 2026-08-27 |
| `/home/ubuntu/nbreq-f2-5-final-launcher.log` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f2-5-final-verify.log` | 0.18 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f2-5-launcher.log` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f2-5-verify.log` | 0.17 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f2-5.pid` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f24-ubuntu-verify.sh` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f3-audit-0222-gate.log` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f3-audit-0222-gate.sh` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f3-audit-0222-install.log` | 0.01 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f3-audit-gate.log` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f3-audit-gate.sh` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f3-audit-install-0221.log` | 0.02 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f3-audit-install-clang.log` | 0.01 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f3-audit-install.log` | 0.03 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f3-package-gate.log` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f3-package-gate.sh` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f3-ubuntu-verify.sh` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f3-verify.log` | 0.26 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f4-1-F87634E7.log` | 0.04 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f4-1-final-004FCFAC.log` | 0.01 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f4-1-final-BEEF28B4.log` | 0.15 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f4-2-FF974A54.exit` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f4-2-FF974A54.log` | 0.15 MiB | 2026-08-29 |
| `/home/ubuntu/nbreq-f4-3-5881F96B.exit` | 0.00 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-f4-3-5881F96B.log` | 0.16 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-f4-4-eef505ba.exit` | 0.00 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-f4-4-eef505ba.log` | 0.16 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-f6-final.log` | 0.14 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-f6-final2.exit` | 0.00 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-f6-final2.log` | 0.15 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-f6-final3.exit` | 0.00 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-f6-final3.log` | 0.15 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-f6-final4.exit` | 0.00 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-f6-final4.log` | 0.15 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-f6-final5.exit` | 0.00 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-f6-final5.log` | 0.15 MiB | 2026-08-30 |
| `/home/ubuntu/nbreq-wp11-4aaa56e.log` | 0.00 MiB | 2026-08-24 |
| `/home/ubuntu/nbreq-wp11-4aaa56e.pid` | 0.00 MiB | 2026-08-24 |
| `/home/ubuntu/nbreq-wp11-4aaa56e.work` | 0.00 MiB | 2026-08-24 |
| `/home/ubuntu/nbreq-wp11.2-4aaa56e-rerun.log` | 0.01 MiB | 2026-08-24 |
| `/home/ubuntu/nbreq-wp11.2-4aaa56e-rerun.pid` | 0.00 MiB | 2026-08-24 |
| `/home/ubuntu/nbreq-wp11.2-de21963.log` | 0.09 MiB | 2026-08-24 |
| `/home/ubuntu/nbreq-wp11.2-de21963.pid` | 0.00 MiB | 2026-08-24 |
| `/home/ubuntu/run-nbreq-f2-5-final.sh` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/run-nbreq-f2-5.sh` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/run-nbreq-f23-final-gate.sh` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/run-nbreq-f23-final-ubuntu.sh` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/run-nbreq-f23-same-batch.sh` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/run-nbreq-f23-ubuntu.sh` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/run-nbreq-f4-1-final-ubuntu.sh` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/run-nbreq-f4-1-final2-ubuntu.sh` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/run-nbreq-f4-1-ubuntu.sh` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/run-nbreq-f4-2-ubuntu.sh` | 0.00 MiB | 2026-08-29 |
| `/home/ubuntu/run-nbreq-f4-3-ubuntu.sh` | 0.00 MiB | 2026-08-30 |
| `/tmp/nbreq-compare-4453bf2.txt` | 0.01 MiB | 2026-08-22 |
| `/tmp/nbreq-d5-forwardport-verify.log` | 0.17 MiB | 2026-08-27 |
| `/tmp/nbreq-dns-final-focused.log` | 0.03 MiB | 2026-08-27 |
| `/tmp/nbreq-dns-final-run.log` | 0.00 MiB | 2026-08-27 |
| `/tmp/nbreq-dns-final-verify.log` | 0.09 MiB | 2026-08-27 |
| `/tmp/nbreq-dns-focused.log` | 0.00 MiB | 2026-08-27 |
| `/tmp/nbreq-dns-green-verify.log` | 0.08 MiB | 2026-08-27 |
| `/tmp/nbreq-dns-green2-focused.log` | 0.00 MiB | 2026-08-27 |
| `/tmp/nbreq-dns-green2-run.log` | 0.00 MiB | 2026-08-27 |
| `/tmp/nbreq-dns-green2-verify.log` | 0.08 MiB | 2026-08-27 |
| `/tmp/nbreq-f22-ef2c5996-race50-fixed.log` | 0.02 MiB | 2026-08-28 |
| `/tmp/nbreq-f22-ef2c5996-race50.log` | 0.10 MiB | 2026-08-28 |
| `/tmp/nbreq-f22-ef2c5996.log` | 0.12 MiB | 2026-08-28 |
| `/tmp/nbreq-f22-race50-launch.log` | 0.00 MiB | 2026-08-28 |
| `/tmp/nbreq-f23-3ebccb72.log` | 0.02 MiB | 2026-08-29 |
| `/tmp/nbreq-f23-fdaddf01.log` | 0.13 MiB | 2026-08-29 |
| `/tmp/nbreq-f23-final-gate-3ebccb72.log` | 0.12 MiB | 2026-08-29 |
| `/tmp/nbreq-f23-final-gate-3ebccb72.nospace.log` | 0.01 MiB | 2026-08-29 |
| `/tmp/nbreq-f23-same-batch-8ecfadbd.log` | 0.14 MiB | 2026-08-29 |
| `/tmp/nbreq-f24-final-verify.log` | 0.16 MiB | 2026-08-29 |
| `/tmp/nbreq-f24-final-verify.pid` | 0.00 MiB | 2026-08-29 |
| `/tmp/nbreq-f24-verify.log` | 0.15 MiB | 2026-08-29 |
| `/tmp/nbreq-f24-verify.pid` | 0.00 MiB | 2026-08-29 |
| `/tmp/nbreq-v010-red-run-1.log` | 0.00 MiB | 2026-08-27 |
| `/tmp/nbreq-v010-red-run-2.log` | 0.00 MiB | 2026-08-27 |
| `/tmp/nbreq-v010-red-run-3.log` | 0.00 MiB | 2026-08-27 |
| `/tmp/nbreq-v010-red-run-4.log` | 0.00 MiB | 2026-08-27 |
| `/tmp/nbreq-v010-red-run-5.log` | 0.00 MiB | 2026-08-27 |
| `/tmp/nbreq-wp10-bear-95b61a6.log` | 2.34 MiB | 2026-08-23 |
| `/tmp/nbreq-wp10-bear-dbe3ff0.log` | 2.88 MiB | 2026-08-23 |
| `/tmp/nbreq-wp9.4j-processes.txt` | 0.00 MiB | 2026-08-22 |

## Removed build/cache directories

These exact paths were removed; no wildcard deletion was used. Allocated-entry totals can overcount hard-linked build outputs, so use the filesystem free-space increase above for recovered space.

| Removed remote path | Allocated entries before deletion |
| --- | ---: |
| `/home/ubuntu/.cache/nbreq-cargo-audit-0222-target` | 695.35 MiB |
| `/home/ubuntu/nbreq-native-target-6eb5206` | 383.74 MiB |
| `/home/ubuntu/nbreq-wp11.2-4aaa56e-run2/target` | 26.69 MiB |
| `/home/ubuntu/nbreq-wp11.2-4aaa56e-run2/tools/xtask/target` | 26.89 MiB |
| `/home/ubuntu/nbreq-wp11.2-de21963-run1/target` | 1401.61 MiB |
| `/home/ubuntu/nbreq-wp11.2-de21963-run1/tools/xtask/target` | 26.89 MiB |
| `/home/ubuntu/nbreq-wp4-072c9d0/target` | 383.64 MiB |
| `/home/ubuntu/nbreq-wp4-bcfae5a/target` | 208.71 MiB |
| `/home/ubuntu/nbreq-wp6-b367247/target` | 283.56 MiB |
| `/home/ubuntu/nbreq-wp8-3223d8e-QQ42Gy/target` | 909.50 MiB |
| `/home/ubuntu/nbreq-wp8-735cb9f-0WPIJj/target` | 825.61 MiB |
| `/home/ubuntu/nbreq-wp8-c1f123e-NlkfxB/target` | 1038.17 MiB |
| `/home/ubuntu/nbreq-wp9-33c43ed-run/nbreq-wp9-33c43ed/target` | 1024.91 MiB |
| `/home/ubuntu/nbreq-wp9-97d3c13-run/nbreq-wp9-97d3c13/target` | 1112.44 MiB |
| `/home/ubuntu/nbreq-wp9.3-bba1d24-run/target` | 1117.49 MiB |
| `/home/ubuntu/nbreq-wp9.4j-proof-d3d2809/target` | 1719.46 MiB |
| `/home/ubuntu/nbreq-wp9.4j-proof-f749df5/target` | 1100.81 MiB |
| `/home/ubuntu/nbreq-wp9.5b-1daedb4-proof/target` | 959.77 MiB |
| `/home/ubuntu/nbreq-wp9.5c-c7467d5-proof/target` | 1193.71 MiB |
| `/home/ubuntu/nbreq/target` | 650.20 MiB |
| `/tmp/nbreq-d4-07128d0-r4.YAzTQf/cargo-target` | 238.06 MiB |
| `/tmp/nbreq-d4-07128d0-r5.zaiUWo/cargo-target` | 1669.94 MiB |
| `/tmp/nbreq-f6-final-target.SMRGI7` | 996.60 MiB |
| `/tmp/nbreq-f6-final2-target.lkLhVI` | 1010.25 MiB |
| `/tmp/nbreq-f6-final3-target.BOvAHO` | 1010.26 MiB |
| `/tmp/nbreq-f6-final4-target.3xIIdn` | 1010.29 MiB |
| `/tmp/nbreq-f6-final5-target.S3cYGp` | 1010.30 MiB |
| `/tmp/nbreq-p10-07-38a4aca.GKyxmY/target` | 372.32 MiB |
| `/tmp/nbreq-p10-07-38a4aca.GKyxmY/tools/xtask/target` | 26.94 MiB |
| `/tmp/nbreq-wp11-4aaa56e.3SrvL3/target` | 26.69 MiB |
| `/tmp/nbreq-wp11-4aaa56e.3SrvL3/tools/xtask/target` | 26.89 MiB |
| `/tmp/nbreq-wp7-724bf65.3rqQ4q/target` | 290.64 MiB |
| `/tmp/nbreq-wp7-81d715e.2SMtRV/target` | 327.45 MiB |
| `/tmp/nbreq-wp7-cc96305.dNj6zk/target` | 327.45 MiB |
| `/tmp/nbreq-wp95-compare-vm6LoE/target` | 1331.30 MiB |
| `/tmp/nbreq-wp95-final-95b61a6/fuzz/target` | 643.56 MiB |
| `/tmp/nbreq-wp95-final-95b61a6/target` | 1327.56 MiB |
| `/tmp/nbreq-wp95-pressure-JccxGg/target` | 1067.45 MiB |

## Evidence and scope

The scan covered nbreq-named entries directly under `/home/ubuntu`, `/home/ubuntu/.cache`, `/tmp`, and `/var/tmp`, then recursively inspected those trees without following symlinks. Other GDS canaries, system/private directories, shared Rust toolchains, and general Cargo caches were not cleanup targets.

Raw inventories, exact deletion plan, script and result are retained locally in `target/linode-cleanup/`. Bridge requests are retained under `C:/User/SecuritasNew/gds/scripts/sessions/putty_bridge/`:

- Initial inventory: `20260905-014341-2cd7c390`.
- Build-cache dry run: `20260905-014647-86043c3b`.
- Deletion: `20260905-014741-1fc1960d`.
- Post-cleanup inventory: `20260905-014821-8d45eb30`.
- Current M0 integrity / old Git status / free space: `20260905-014848-a09a7de3`.

Deletion plan SHA-256: `958a45bbc3d1f98b79823fa67c76a8fdeb0b1d73f3cb67b8dcda543bfe745588`. The remote helper recorded its result in `/tmp/nbreq-cleanup-build-result-20260905.json`. These ignored raw artifacts may need regeneration; the review list above is the durable record.
