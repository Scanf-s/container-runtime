# Verify the isolation

[Getting started](../README.md) · [Learning notes](README.md)

Run these commands inside the container shell.

```bash
ip link # isolated network namespace (only lo - loopback device occurs)
```
<img width="490" height="61" alt="스크린샷 2026-07-11 235123" src="https://github.com/user-attachments/assets/7958bf35-77a5-4b23-b5fa-5fe71b771fe7" />

```bash
mount   # isolated rootfs and procfs
ls -al
```
<img width="495" height="379" alt="스크린샷 2026-07-11 235128" src="https://github.com/user-attachments/assets/2382666b-0ec4-4f8e-b806-caa73d5fcc5c" />

```bash
ps -a   # only container-local processes
```
<img width="235" height="76" alt="스크린샷 2026-07-11 235133" src="https://github.com/user-attachments/assets/a182b541-9cc1-48db-947c-e5378a03d177" />

```bash
id      # uid=0(root), gid=0(root)
```
<img width="360" height="39" alt="스크린샷 2026-07-11 235138" src="https://github.com/user-attachments/assets/647989a8-13c2-4b4a-b7f2-010876acfd3e" />
