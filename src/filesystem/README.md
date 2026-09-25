## What's going on here

- rootfs is the directory that you downloaded through ./scripts/fetch-rootfs.sh
- The goal of this module is to make that directory appear as '/' to the container process.

To do this, we need to follow the steps as below.

1. Create a mount namespace

unshare() systemcall with `CLONE_NEWNS` flag gives this process its own view of mounts.
By using this systemcall, the process could seperate a view from the host system does.

- CLONE_NEWNS로 unshare() 호출 시 현재 프로세스에 새 마운트 네임스페이스가 생성됨
- 이 새 마운트 네임스페이스는 호스트의 `mount table` = '/'에는 뭐가 보이고, `/proc` 에는 뭐가 보이고... 를 기록해둔 테이블
- 이 mount table을 그대로 복사해서 새로운 마운트 테이블을 가지게 됨 (호스트와 동일한 마운트 테이블 내용)
- 이 새로운 마운트 테이블에다가 mount propagation 설정을 함. -> 왜? 두 배치표는 복사된 것이긴 한데, 리눅스 시스템에서는 한쪽에서 변경된 내용을 다른쪽에도 반영을 하는 구조로 되어있음
- 따라서 mount propagation 설정을 `MS_PRIVATE`으로 설정해야지 새로운 마운트 테이블에서 변경된 사항이 호스트에도 전파가 안됨
    - propagation 설정은 부모의 설정을 그대로 받아버리는데, 만약 부모의 option이 MS_SHARED라면....
    - 만약 `MS_SHARED` 상태로 새 마운트 테이블에 변경을 해버리면? -> 호스트 마운트 테이블에도 영향이 감
    - 이 경우 무슨 일이 발생하냐? -> rootfs에 /proc이라는걸 새 프로세스에서 마운트해버리면 -> 호스트에서도 rootfs/proc 경로가 보임
    - 그래서 확실하게 Isolated environment를 만들기 위해 `MS_PRIVATE`으로 일단 설정해버리는 것.

- 그 이후, rootfs 경로를 마운트 경로로, 새로운 마운트 테이블에 설정하기 위해 mount()와 MS_BIND 플래그를 호출함
    - MS_BIND 플래그는 새로운 마운트 경로를 만들때 사용

-

2. Make mounts private

- Ref: https://man7.org/linux/man-pages/man2/mount.2.html

> mount() attaches the filesystem specified by `source` to the location specified by the pathname in `target`

source: which one are you going to attach?
target: where the souce is going to be mounted?
