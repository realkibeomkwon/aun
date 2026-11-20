# Aun 다음 작업 (야간 배치)

작성: 2026-11-20. 기준 커밋: `5d51dbd` (`main`).
대상: 사람 없이 밤새 진행하는 Claude 세션. 사용자는 08:00에 결과를 확인한다.

## 현재 상태

MVP는 Mac(Kibeoms-Mac-mini)과 Windows(KIBEOMKWON) 실기기에서 모두 확인했다.
탐색(UDP 47811), 6자리 코드 페어링(SPAKE2 + Noise XX), 암호화 링크(TCP 47810), 클립보드 텍스트 동기화,
파일 전송, 창을 닫은 뒤 백그라운드 동작이 동작한다. 구조와 프로토콜은 `README.md`에 있다.

- `crates/aun-core`: UI 없는 엔진. `cargo test -p aun-core`
- `app`: rune UI. `cargo test -p aun`, `cargo run -p aun -- --snap out.png --scene demo` (헤드리스 렌더는 debug 빌드에서만 된다)
- 링크 로그: `~/Library/Application Support/Aun/aun.log`, `%APPDATA%\Aun\aun.log`
- 사용자의 우선순위: **성능, 속도, 낮은 메모리 사용량, 최적화**. 대상은 macOS와 Windows뿐이다(모바일, Linux 제외).

## 작업 규칙

1. 새 브랜치 `overnight/2026-11-21`에서 작업한다. **`main`에 병합하거나 `main`에 푸시하지 않는다.** 브랜치 푸시는 해도 된다(CI 확인용).
2. 작업 하나마다 커밋 하나. 커밋 전에 `cargo clippy --workspace --all-targets` 경고 0, `cargo test --workspace` 통과.
3. `~/.claude/CLAUDE.md` 규칙을 따른다. 특히: 원인을 추측하지 말고 재현한 뒤 고친다. 테스트는 실제 버그나 요구사항에서 실패해야 한다. 기존 테스트를 약하게 만들거나 지우지 않는다.
4. 새 의존성은 꼭 필요할 때만 추가한다. rune의 `Cargo.lock`에 이미 있는 크레이트와 버전을 먼저 쓴다(`~/.cargo/git/checkouts/rune-*/07f1ddd/Cargo.lock`).
5. UI를 바꾸면 헤드리스 PNG로 확인하고 접근성 경고(`rune: a focusable ...`)를 남기지 않는다.
6. Windows 전용 코드는 이 Mac에서 실행할 수 없다. 작업 A의 CI로 컴파일과 테스트를 확인하고, 실기기 동작은 "미확인"으로 보고한다.
7. 한 작업이 두 번 막히면 그 작업을 건너뛰고 이유를 보고서에 남긴 뒤 다음 작업으로 간다.
8. 진행 상황은 `docs/overnight-report.md`에 작업마다 갱신한다(완료/부분/건너뜀, 커밋, 검증한 것, 미확인인 것). 세션이 중간에 끊겨도 남도록 작업마다 커밋한다.
9. 사용자의 실행 중인 Aun(Mac, 스크래치 폴더의 `Aun.app`)과 `~/Library/Application Support/Aun`의 페어링 정보는 건드리지 않는다. 테스트는 임시 폴더와 `port: 0`, `discovery: false`로 한다.

## 작업 목록 (우선순위 순)

### A. CI: macOS와 Windows에서 빌드와 테스트

이 Mac에서는 Windows용 앱을 컴파일할 수 없다(rune의 stacker/psm이 Windows SDK 헤더를 요구). CI가 있으면 이후 모든 작업의 Windows 검증이 된다.

- `.github/workflows/ci.yml`: `macos-latest`, `windows-latest`에서 `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo build --release -p aun`.
- 툴체인은 `rust-toolchain.toml`(1.99.1)을 따른다. `Swatinem/rust-cache`로 캐시한다. rune 첫 빌드가 길다(Windows release 약 6분 30초).
- `throughput` 테스트(`#[ignore]`)는 release로 따로 돌려 MB/s를 로그에 남긴다.
- 완료 기준: 브랜치를 푸시하고 `gh run watch`로 두 OS 모두 통과를 확인한다. 실패하면 로그를 읽고 고친다.

### B. 로그인 시 자동 실행과 트레이 아이콘

매일 쓰려면 가장 먼저 필요하다. 지금은 창을 닫으면 다시 실행해야만 창이 돌아온다.

- macOS: 메뉴 막대 상태 아이템(열기, 클립보드 공유 켜기/끄기, 종료). `App::accessory()`로 Dock 아이콘 없이 상주하는 옵션.
- Windows: 알림 영역 트레이 아이콘(같은 메뉴).
- rune은 트레이를 제공하지 않는다(`crates/rune/README.md`의 "Compact tray-controlled windows"). `AppHandle`은 다른 스레드에서 `open()`, `quit()`, `update()`를 부를 수 있다. 크레이트는 `tray-icon`을 우선 검토하되, rune이 이미 쓰는 `objc2-app-kit`, `windows-sys`로 직접 만드는 쪽이 더 가벼우면 그쪽을 택하고 이유를 보고서에 쓴다.
- 로그인 시 자동 실행: macOS는 LaunchAgent plist(`~/Library/LaunchAgents/dev.aun.desk.plist`) 또는 `SMAppService`, Windows는 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. UI에 켜기/끄기 토글을 둔다.
- 설정(클립보드 공유 여부, 자동 실행)을 설정 폴더에 저장한다. 지금은 클립보드 토글이 재시작하면 초기화된다.
- 완료 기준: 설정 저장과 로드 테스트, 헤드리스 렌더, macOS에서 실제 실행해 상태 아이템과 메뉴가 동작하는지 확인. Windows는 CI 빌드만.

### C. 클립보드 보안: 비밀번호는 동기화하지 않기

지금은 비밀번호 관리자에서 복사한 비밀번호도 상대 기기로 간다.

- macOS: 페이스트보드에 `org.nspasteboard.ConcealedType` 또는 `org.nspasteboard.TransientType`이 있으면 보내지 않는다.
- Windows: `ExcludeClipboardContentFromMonitorProcessing` 형식이 있거나 `CanIncludeInClipboardHistory`가 0이면 보내지 않는다.
- `crates/aun-core/src/clip.rs`에 플랫폼별 판정 함수를 두고, 판정 로직은 테스트 가능한 형태로 분리한다.
- 완료 기준: macOS에서 `NSPasteboard`에 concealed 형식을 넣는 테스트(사용자의 클립보드를 건드리지 않도록 이름 있는 별도 페이스트보드 사용). Windows는 CI 컴파일만.

### D. 클립보드 이미지 동기화

- 스크린샷 같은 이미지(PNG)를 양방향으로 동기화한다. 새 프레임 종류(`ClipImage`)를 추가하고 `MAX_FRAME`(4 MiB)을 넘는 이미지는 보내지 않거나, 필요하면 프레임 상한을 이미지에만 올린다(근거를 보고서에).
- `arboard`의 `image-data` 기능을 켠다. 인코딩은 rune이 이미 쓰는 `png` 크레이트로 한다.
- 성능: 변경 카운터가 바뀐 경우에만 읽는다(지금 구조 유지). 큰 이미지 인코딩은 클립보드 스레드를 막지 않게 한다.
- 완료 기준: 프레임 왕복 테스트, 인코딩/디코딩 테스트, 크기 상한 테스트.

### E. 파일 받기 개선

- 받은 파일 줄에 "Finder에서 보기"/"탐색기에서 보기" 버튼(macOS `open -R`, Windows `explorer /select,`).
- 진행 중인 전송 취소 버튼(보내는 쪽 `FileAbort`, 받는 쪽은 `.part` 삭제). 받는 쪽도 취소할 수 있게 새 프레임이 필요하면 추가한다.
- 저장 폴더 선택(`tx.open_folder`), 설정에 저장.
- 여러 파일을 한 번에 끌어다 놓을 때 진행률이 한 줄씩 정상 표시되는지 확인.
- 완료 기준: 취소 시 `.part`가 남지 않는 통합 테스트, 헤드리스 렌더.

### F. 연결 견고성

- 탐색: 인터페이스별 directed broadcast(지금은 255.255.255.255 하나). 이더넷과 Wi-Fi를 둘 다 쓰는 Mac에서 필요하다. 인터페이스 목록은 `getifaddrs`(macOS), `GetAdaptersAddresses`(Windows).
- 수동 연결: 탐색이 막힌 네트워크용으로 IP를 직접 입력해 페어링하는 입력란.
- 절전 해제 후 재연결이 빠른지 확인(지금은 35초 무응답 후 끊기고 3초마다 재시도).
- 완료 기준: 인터페이스 목록과 브로드캐스트 주소 계산 테스트.

### G. 성능과 메모리 측정

사용자의 최우선 요구사항이다. 숫자로 남긴다.

- 기준 측정: 창을 연 상태와 닫은 상태(`release_gpu_when_closed`)의 메모리(`footprint`), 유휴 CPU, 링크 1개 유지 시 초당 깨어나는 횟수.
- 루프백 전송 속도(지금 Mac release 330 MB/s). 병목을 프로파일링하고(`cargo instruments` 또는 `samply`), 개선 여지가 있으면 고친다. 예: 청크 크기, `BufWriter` 크기, 암호화 버퍼 복사, `fsync` 위치.
- 클립보드 300 ms 폴링을 OS 알림으로 바꿀 가치가 있는지 측정으로 판단한다(macOS는 알림이 없어 폴링이 표준이다. Windows는 `AddClipboardFormatListener`).
- 완료 기준: 측정 전후 숫자를 보고서에 표로 남긴다. 개선이 없으면 바꾸지 않는다.

### H. (시간이 남으면) 마우스·키보드 공유(KVM) 설계

구현하지 말고 설계 문서만 쓴다. 실기기 검증 없이 입력을 가로채는 코드는 위험하다.

- `docs/kvm-design.md`: 화면 가장자리 판정, macOS 입력 캡처(`CGEventTap`, 접근성 권한)와 주입(`CGEventPost`), Windows 캡처(`SetWindowsHookEx` 저수준 훅)와 주입(`SendInput`), 프로토콜 프레임(상대 이동량 누적, 키 코드 매핑), 지연 목표, 실패 시 입력을 되돌려 주는 안전장치.
- 참고: TwinDesk의 `docs/protocol.md`(tdv1/tdv2)와 `docs/full-kvm.md`. 코드는 복사하지 않는다(GPL-3.0).

## 아침 보고 형식

`docs/overnight-report.md` 맨 위에 요약 표를 둔다.

| 작업 | 상태 | 커밋 | 검증 | 미확인 |
|---|---|---|---|---|

그 아래 작업마다 바뀐 것, 확인한 것, 확인하지 못한 것, 사용자가 실기기에서 해 볼 것을 적는다.
