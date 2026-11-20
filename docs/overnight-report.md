# 야간 배치 보고 (2026-11-21)

브랜치: `claude/overnight-a68e04` (문서의 `overnight/2026-11-21` 대신). `main`은 건드리지 않았다.
CI: <https://github.com/realkibeomkwon/aun/actions?query=branch%3Aclaude%2Fovernight-a68e04>

**먼저 알아 둘 것**: 새 프레임(클립보드 이미지, 전송 취소)이 생겨 이전 빌드와는 링크가 유지되지 않는다. Mac과 Windows를 **함께** 이 브랜치로 업데이트해야 한다. 페어링은 그대로 남는다. Windows 실행 파일은 CI 실행의 `aun-windows-latest` 아티팩트로 받을 수 있다.

| 작업 | 상태 | 커밋 | 검증 | 미확인 |
|---|---|---|---|---|
| A. CI | 완료 | `92dcd33` | 두 OS에서 clippy·테스트·release 빌드·처리량 통과 | |
| B. 트레이·자동 실행 | 완료 | `0774514` | 설정 저장/로드, LaunchAgent(`plutil -lint`), 헤드리스 PNG, macOS 메뉴 막대 아이콘 표시 | 메뉴 항목 클릭(접근성 권한 없음), Windows 트레이·Run 값 실기기 |
| C. 비밀번호 클립보드 제외 | 완료 | `5c4a2c9` | 이름 있는 페이스트보드로 Concealed/Transient 판정, Windows 플래그 판정 | 실제 비밀번호 관리자에서 복사 |
| D. 클립보드 이미지 | 완료 | `10ed197` | 프레임 왕복, 종류별 크기 상한, PNG 왕복, 압축 폭탄 거부, 실제 스크린숏 인코딩 시간 | 두 기기 사이 실제 이미지 복사 |
| E. 파일 받기 개선 | 완료 | `77e084f`, 테스트 `de08718` | 취소 양방향 통합 테스트(`.part` 없음, 링크 유지), 80개 동시 전송, 스크롤, 헤드리스 PNG | "Finder/탐색기에서 보기" 실제 클릭, 폴더 선택 대화상자 |
| F. 연결 견고성 | 완료 | `ebab287`, 테스트 `4834688` | 브로드캐스트 주소 계산, 이 Mac 인터페이스 목록, 주소로 페어링한 기기의 자동 재연결(탐색 끔), 주소 입력 UI | 실제 두 네트워크, 실제 절전/해제, Windows `GetAdaptersAddresses` 실행(CI만) |
| G. 성능·메모리 측정 | 완료 | `98e70c7`, `b39fee2` | 프로파일(암호화가 바쁜 샘플의 약 80%), ChaCha20 NEON으로 Mac 봉인 +25~40%, AES-GCM 시험 후 되돌림(Windows에서 느림), RFC 8439 알려진 답 테스트, 메모리·CPU·깨어남 측정 | Windows 실기기 속도·메모리, 실제 네트워크 속도, 메인 스레드 깨어남의 출처 |
| H. KVM 설계 | 완료(문서) | `ab9068b` | `docs/kvm-design.md` | 구현 없음(설계 문서만) |

보고서 커밋: `0c62472`, `844b2a0`, `a1be5b3`, `05974d5`, `3a94b8f`, 그리고 이 표를 고친 마지막 커밋. 테스트만 고친 커밋은 E·F 줄에 함께 적었다(`de08718`: E의 취소·다중 전송 테스트가 느린 CI에서 오래된 알림을 잡던 문제, `4834688`: 기존 상태 메시지 테스트가 Windows CI의 15초 첫 프레임에 걸리던 문제).

## A. CI

- 바뀐 것: `.github/workflows/ci.yml`. `macos-latest`, `windows-latest`에서 `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo build --release -p aun`, release `throughput` 테스트, 실행 파일을 아티팩트로 올린다. 툴체인은 `rust-toolchain.toml`(1.99.1)을 rustup이 읽는다. 캐시는 `Swatinem/rust-cache@v2`.
- 확인한 것: 첫 실행(캐시 없음) 두 OS 모두 성공. 걸린 시간은 Windows 10분 23초, macOS 12분 59초.

  | 단계 | Windows | macOS |
  |---|---|---|
  | clippy | 85 s | 88 s |
  | test | 149 s | 154 s |
  | release 빌드 | 313 s | 439 s |
  | 처리량(루프백 512 MiB) | 163 MB/s | 118 MB/s |

  CI 러너의 처리량은 디스크가 느려서 이 Mac(330 MB/s)보다 낮다. 추세 비교용으로만 본다.
- 확인하지 못한 것: 없음.
- 사용자가 해 볼 것: 없음. 이후 커밋마다 Actions 탭에서 두 OS 결과를 본다. Windows 실행 파일은 실행 결과의 `aun-windows-latest` 아티팩트로 받을 수 있다.

## B. 로그인 시 자동 실행과 트레이 아이콘

- 바뀐 것
  - `app/src/tray.rs`: Windows는 rune의 `rune::Tray`(`Shell_NotifyIcon`, 자체 스레드)를 쓴다. rune에는 macOS 백엔드가 없어서, macOS는 `objc2-app-kit`으로 `NSStatusItem`을 직접 만든다(SF Symbol `rectangle.on.rectangle`, 템플릿 이미지). 메뉴는 "Aun 열기", "클립보드 공유 켜기/끄기"(체크 표시), "종료". `tray-icon` 크레이트는 쓰지 않았다. rune이 이미 Windows 트레이를 갖고 있고 macOS 쪽은 rune이 이미 빌드하는 `objc2` 0.6 / `objc2-app-kit` 0.3.2로 100줄 안쪽이라, 새 크레이트(`tray-icon`은 `muda` 등을 끌어온다)가 필요 없었다. Windows 아이콘은 32×32 RGBA를 코드로 그린다(파란 둥근 사각형에 겹친 화면 두 개).
  - `app/src/autostart.rs`: macOS는 `~/Library/LaunchAgents/dev.aun.desk.plist`(경로는 `$HOME` 기준, `RunAtLoad`, `LimitLoadToSessionType=Aqua`), Windows는 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`의 `Aun` 값(`"경로" --background`). 켜짐 여부는 저장하지 않고 시스템에서 읽는다. 사용자가 시스템 설정에서 지우면 앱에도 꺼짐으로 보인다. 앱이 시작할 때 켜져 있으면 현재 실행 파일 경로로 다시 써서, 앱을 옮겨도 따라간다. `launchctl load`는 하지 않는다(지금 하나 더 뜨므로).
  - `--background`로 시작하면 창 없이(`App::windowless`) 뜬다. 로그인 시 실행이 이 인자를 쓴다.
  - `app/src/settings.rs`: 설정 폴더의 `settings.txt`(`clipboard=1`, `dock=1`). 모르는 키는 무시해 이후 키를 추가해도 호환된다. 클립보드 공유는 재시작해도 유지되고, `Config.clipboard`도 이 값으로 시작한다.
  - Dock 아이콘 숨김(macOS): 설정이 꺼져 있으면 `App::accessory()`로 시작하고, 실행 중에는 `AppHandle::set_dock_icon`으로 바꾼다. accessory 앱에서는 rune 기본 reopen이 아무것도 하지 않으므로 `on_reopen`에서 창을 연다.
  - 클립보드 공유는 헤더 버튼과 트레이 메뉴 모두 `Ui::set_clip` 한 경로로 바뀐다(노드, 설정 파일, 트레이 체크 표시를 함께 갱신).
  - UI: 헤더 아래에 "로그인 시 실행", "Dock 아이콘 표시/숨김"(macOS만) 버튼 줄.
- 확인한 것
  - 테스트: 설정 저장/로드(`settings_survive_a_restart`), 헤더 버튼 클릭 후 다음 시작에서 꺼짐이 유지됨(`the_clipboard_switch_is_remembered_across_restarts`), LaunchAgent가 `plutil -lint`를 통과하고 `&`, `<>`가 든 경로가 그대로 읽히며 끄면 지워짐, 트레이 아이콘 픽셀 크기. Windows Run 값 테스트(`HKCU\Software\AunTest-<pid>` 임시 키에 쓰고 읽고 지움)는 CI Windows에서 돈다.
  - 헤드리스 PNG: 새 버튼 줄이 나오고 `rune: a focusable ...` 경고 없음.
  - macOS 실제 실행(`HOME=<스크래치>`로 띄워 사용자의 Aun과 설정을 건드리지 않음): 로그에 `tray up`. 메뉴 막대를 캡처해 비교하니 우리 프로세스가 떠 있을 때만 겹친 사각형 아이콘이 보이고, 끄면 사라졌다. `dock=0`과 `--background`로 띄우면 화면에 창이 없고 앞 앱의 메뉴 막대를 가져가지 않았다.
- 확인하지 못한 것
  - 메뉴 항목 클릭: 이 세션에 접근성 권한이 없어 System Events가 시간 초과로 끝났다. 클릭 → `pick` → `AppHandle` 호출 경로는 코드로만 확인.
  - Dock 숨김 상태에서 Finder로 다시 실행했을 때 창이 열리는지(`on_reopen`): LaunchServices로 띄운 번들이 아니라 확인 불가.
  - Windows 트레이 아이콘과 Run 값의 실기기 동작: CI 빌드와 레지스트리 테스트만.
  - `dock=0` 실행 캡처에서는 메뉴 막대 관리 앱(겹친 아이콘을 «로 접는 앱)이 아이콘을 접어서 보이지 않았다. 로그에는 `tray up`.
- 사용자가 해 볼 것
  1. Mac: 메뉴 막대 아이콘 → "클립보드 공유 끄기"를 누르고 창의 헤더 버튼이 꺼짐으로 바뀌는지, 앱을 다시 켜도 꺼짐인지.
  2. Mac: "로그인 시 실행"을 켜고 `~/Library/LaunchAgents/dev.aun.desk.plist`가 생기는지, 로그아웃/로그인 후 창 없이 메뉴 막대에만 뜨는지.
  3. Mac: "Dock 아이콘 숨김" 후 창을 닫고 Spotlight로 Aun을 다시 실행하면 창이 열리는지.
  4. Windows: 알림 영역 아이콘 클릭으로 창이 열리는지, 오른쪽 클릭 메뉴 세 항목, "로그인 시 실행"을 켠 뒤 작업 관리자 › 시작 앱에 Aun이 보이는지.

## C. 클립보드 보안: 비밀번호는 동기화하지 않기

- 바뀐 것: `crates/aun-core/src/clip.rs`. 변경 카운터가 움직였을 때 내용을 읽기 전에 `secret()`을 본다. 비밀이면 읽지도 보내지도 않는다.
  - macOS: 일반 페이스트보드 형식에 `org.nspasteboard.ConcealedType` 또는 `org.nspasteboard.TransientType`이 있으면 비밀(`secret_types`).
  - Windows: `ExcludeClipboardContentFromMonitorProcessing`이 있거나, `CanIncludeInClipboardHistory` 또는 `CanUploadToCloudClipboard`가 0이면 비밀(`secret_flags`). 마지막 것은 문서에 없던 항목이지만 "다른 기기로 보내지 말라"는 뜻이라 Aun에 그대로 해당해서 넣었다. 형식은 있는데 값을 못 읽으면(클립보드를 못 열면) 비밀로 본다. 한 번 놓치는 편이 새는 것보다 낫다.
- 확인한 것: macOS 테스트는 이름 있는 별도 페이스트보드(`dev.aun.test.<pid>`)에 평문과 두 형식을 넣어 판정을 확인하고 지운다(사용자 클립보드는 그대로). 판정 규칙 테스트. Windows 테스트(`marked_windows_copies_are_secret`)는 `CI` 환경 변수가 있을 때만 러너의 실제 클립보드에 형식을 넣어 `secret()`을 확인한다.
- 확인하지 못한 것: 실제 비밀번호 관리자(1Password, Bitwarden, KeePass 등)에서 복사했을 때. 각 앱이 어떤 형식을 붙이는지는 앱마다 다르다.
- 사용자가 해 볼 것: 쓰는 비밀번호 관리자에서 비밀번호를 복사하고 상대 기기 클립보드가 바뀌지 않는지, 일반 텍스트는 여전히 가는지.

## D. 클립보드 이미지 동기화

- 바뀐 것
  - 새 프레임 `ClipImage`(종류 9, PNG). `MAX_FRAME`(4 MiB)은 그대로 두고 이미지만 16 MiB(`MAX_IMAGE`)까지 받는다. `read_frame`은 길이 다음 종류 바이트를 먼저 읽고 그 종류의 상한을 넘으면 본문을 할당하기 전에 거부한다. 근거: Retina 스크린숏 PNG는 4 MiB를 자주 넘고, 다른 프레임의 상한을 올리면 인증 전 상대도 큰 할당을 일으킬 수 있다.
  - `arboard`의 `image-data` 기능을 켰다(rune이 이미 `image` 크레이트를 빌드하므로 새 크레이트 없음). PNG는 rune이 쓰는 `png` 0.18, 압축은 `Compression::Fast`(fdeflate).
  - 로컬 복사: 텍스트를 먼저 보고, 텍스트가 없을 때만 이미지를 읽는다(엑셀처럼 둘 다 있으면 텍스트). 인코딩은 `aun-png` 스레드에서 해 클립보드 스레드를 막지 않는다. 인코딩 중에 새 복사가 오면 오래된 이미지는 버린다(세대 번호). 16 MiB를 넘는 PNG는 보내지 않고 로그만 남긴다.
  - 받는 쪽: 클립보드 스레드에서 디코딩한다. 디코딩한 픽셀이 128 MiB(약 5800×5800)를 넘으면 거부한다. 테스트를 쓰다가 `png`의 `Limits`가 내부 버퍼만 제한하고 출력 버퍼에는 적용되지 않는 것을 발견해, 출력 크기를 직접 확인하도록 고쳤다(`a_png_claiming_a_huge_canvas_is_refused`가 이 버그에서 실패했다).
  - 되돌아오는 복사 방지는 텍스트와 같은 방식(받아서 쓴 직후 변경 카운터 갱신, 내용 해시 비교).
- 확인한 것: 프레임 왕복, 텍스트 프레임 4 MiB 초과와 이미지 16 MiB 초과는 헤더만으로 거부, 4 MiB를 넘는 이미지 프레임은 통과. PNG 왕복(2880×1800 픽셀 일치). 측정(release, 이 Mac):

  | 이미지 | RGBA | PNG | 인코딩 | 디코딩 |
  |---|---|---|---|---|
  | 실제 화면 캡처 1920×1080 | 8.1 MB | 802 KB | 9.6 ms | 11.7 ms |
  | 합성 2880×1800 | 20.7 MB | 238 KB | 6.9 ms | 6.5 ms |

- 확인하지 못한 것: 두 기기 사이 실제 이미지 복사(클립보드 스레드는 실제 시스템 클립보드를 쓰므로 테스트에서 돌리지 않았다). `arboard::get_image` 자체의 시간(macOS는 TIFF→RGBA 변환).
- 사용자가 해 볼 것: Mac에서 ⌘⇧⌃4로 영역 캡처 → Windows에서 그림판에 붙여넣기. 반대로 Windows에서 Win+Shift+S → Mac 미리보기 › 클립보드에서 새로 만들기. `aun.log`에 `clipboard image WxH to N KB PNG in M ms`가 남는다.

## E. 파일 받기 개선

- 바뀐 것
  - **버그 수정(재현 후)**: 파일 여러 개를 한 번에 끌어다 놓으면 파일마다 보내는 스레드가 따로 떠서 동시에 열린 파일이 받는 쪽 상한(64개)을 넘었고, 받는 쪽이 `PermissionDenied: too many files`로 링크를 끊었다. 80개 1 MiB 파일로 재현했을 때 1개만 도착했다(`many_files_dropped_at_once_all_arrive`, 수정 전 실패 로그는 커밋 메시지에). 이제 상대 기기마다 대기열(`Outbox`) 하나와 보내는 스레드 하나가 파일을 하나씩 보낸다. 끌어다 놓은 파일은 모두 바로 "0%" 줄로 보인다. 한 TCP 링크 위에서 동시에 보내도 속도 이득이 없으므로 순차 전송이 손해가 아니다.
  - 취소: 진행 중인 줄에 "취소" 버튼. 보내는 쪽이 취소하면 `FileAbort`를 보내고, 받는 쪽이 취소하면 `.part`를 바로 지우고 새 프레임 `FileCancel { id }`(종류 10)로 알린다. 보내는 쪽은 그 링크의 상대가 보낸 것이고 지금 보내는 파일 id와 맞을 때만 멈춘다. 받는 스레드가 막히지 않도록 `FileCancel`은 `try_send`로 보내고, 큐가 차 있으면 다음 프레임 때 다시 보낸다. 대기열에 있던 파일을 취소하면 시작하지 않는다.
  - "Finder에서 보기"/"탐색기에서 보기": 받은 파일이 완료된 줄에 버튼. macOS `open -R <경로>`, Windows `explorer /select,"<경로>"`(`raw_arg`로 따옴표를 그대로 넘긴다). `Note::Transfer`에 `path`와 `cancelled`가 추가됐다.
  - 저장 폴더: "받은 파일: ~/Downloads" 줄의 "폴더 변경" → `tx.open_folder`. `settings.txt`의 `downloads=`에 저장하고, 실행 중에는 `Node::set_downloads`로 바로 바뀐다(이미 받는 중인 파일은 원래 폴더에서 끝난다).
  - 줄 목록: 끝나지 않은 전송은 모두, 끝난 전송은 최근 6개를 남기고 최대 12줄을 그린다. 창 내용이 창 높이를 넘으면 스크롤된다(이번 작업으로 데모 화면이 560pt를 넘었다).
- 확인한 것
  - `a_cancelled_transfer_leaves_no_part_file`: 4 GiB 희소 파일을 보내다 받는 쪽 취소, 다시 보내는 쪽 취소. 두 경우 모두 `.part`와 완성 파일이 남지 않고, 양쪽에 `cancelled` 알림이 오고, 링크가 유지되고, 바로 다음 작은 파일이 도착한다.
  - `many_files_dropped_at_once_all_arrive`: 80개가 모두 실패 없이 도착하고 링크 유지. 수정 전에는 1/80.
  - `transfers_past_the_window_bottom_scroll_into_view`: 스크롤 없이 실패(마지막 줄 y=565)하고, 스크롤로 통과하는 것을 확인했다.
  - 헤드리스 PNG에 취소 버튼, Finder에서 보기 버튼, 폴더 줄이 보이고 접근성 경고 없음.
- 확인하지 못한 것: "Finder/탐색기에서 보기" 클릭(사용자 화면에 Finder 창이 뜨므로 실행하지 않았다), 폴더 선택 대화상자, Windows `explorer /select,` 동작(CI 컴파일만), 보내는 쪽이 취소했을 때 받는 쪽 줄은 "취소됨"이 아니라 "실패"로 보인다(`FileAbort`가 읽기 오류와 같은 프레임이라 구분하지 않았다).
- 사용자가 해 볼 것: 큰 파일을 보내는 중에 받는 쪽에서 "취소" → 받은 폴더에 `.part`가 없는지. 파일 20~30개를 한꺼번에 끌어다 놓기. 받은 줄의 "Finder에서 보기". "폴더 변경"으로 바탕화면을 고르고 다시 받기, 앱을 껐다 켜도 유지되는지.

## F. 연결 견고성

- 바뀐 것
  - 인터페이스별 directed broadcast: 3초마다 255.255.255.255에 더해, 켜져 있고 루프백이 아닌 IPv4 인터페이스마다 `주소 | !넷마스크`로 보낸다. 목록은 macOS `getifaddrs`, Windows `GetAdaptersAddresses`. /31, /32(점대점, Tailscale 같은 VPN)는 브로드캐스트가 없으므로 뺀다. 목록은 매번 다시 읽어 Wi-Fi 변화를 따라간다.
  - IP로 페어링: 기기 목록 아래 "목록에 없나요? IP 주소로 페어링" 입력란. `192.168.0.5` 또는 `주소:포트`. 페어링 박스가 같은 코드 입력 흐름으로 열린다. 탐색이 보지 못한 기기를 주소로 페어링하면, 건 쪽이 그 주소를 `peers.txt` 셋째 칸에 저장하고(예전 형식 파일도 그대로 읽는다), 링크가 끊겨 있으면 5초마다 다시 건다(`aun-keep` 스레드).
  - 절전 해제: 같은 스레드가 5초마다 벽시계(`SystemTime`)와 단조 시계(`Instant`)의 경과 차이를 본다. 벽시계가 10초 넘게 앞서면(macOS `Instant`는 `CLOCK_UPTIME_RAW`라 잠자는 동안 멈춘다) 잠들었다 깬 것으로 보고 모든 링크를 닫는다. 그러면 다음 탐색 알림(3초 이내)이나 재다이얼이 바로 새 링크를 만든다.
  - 이것이 필요한 이유(코드로 분석, 실기기 미재현): 잠든 쪽은 깨어난 뒤에도 죽은 링크를 들고 있어 35초 무응답 전까지 다시 걸지 않는다. 그 사이 상대가 걸어 오면, 기존 링크를 건 쪽의 id가 더 작을 때 새 링크가 중복으로 버려진다(`dup` 규칙). 그래서 깨어난 뒤 최대 35초 이상 끊길 수 있다.
- 확인한 것
  - `directed_broadcasts_set_the_host_bits`, `this_machine_has_a_broadcast_address`: 이 Mac은 `[(192.168.0.4, /24), (192.168.0.8, /24), (100.98.146.73, /32)]` → `[192.168.0.255]`(이더넷과 Wi-Fi가 같은 서브넷, Tailscale은 제외). CI 러너에서도 같은 테스트가 돈다.
  - `a_device_paired_by_address_reconnects_without_discovery`: 탐색을 끈 두 노드를 주소로 페어링하면 `peers.txt`에 주소가 남고, 게스트를 다시 시작하면 아무 호출 없이 다시 연결된다(변경 전에는 연결되지 않아 시간 초과로 실패한다).
  - `a_typed_address_opens_the_code_box_for_it`: 입력란 클릭 → 주소 입력 → 페어링 버튼 → 코드 박스가 그 주소로 열림. `typed_addresses_take_aun_s_port_unless_given`, `a_sleep_shows_as_wall_time_running_ahead`.
  - `peers.txt` 새 형식과 예전 형식 읽기(`identity_and_peers_persist`에 추가).
- 확인하지 못한 것: 서로 다른 두 네트워크(이더넷/Wi-Fi가 다른 서브넷)에서 실제 탐색, 실제 절전과 해제, Windows에서 `Instant`가 잠자는 동안 멈추는지(멈추지 않으면 Windows에서는 감지가 일어나지 않고 예전처럼 35초 무응답으로 끊긴다), Windows `GetAdaptersAddresses` 결과(CI 테스트로만).
- 사용자가 해 볼 것: Mac을 잠자기 → 1분 뒤 깨우고 Windows 쪽 "연결됨"까지 걸리는 시간(로그 `woke after N s asleep: closing links`, `link ... up`). Windows 노트북을 잠자기/깨우기로도 같은 시간 확인. Tailscale 주소(100.x)를 입력해 페어링해 보기(같은 LAN이 아니어도 된다).

### CI에서 고친 테스트

- `a_status_message_goes_away_on_its_own`(기존 테스트)이 Windows CI 한 번(작업 D 커밋)에서 실패했다. 로그 타임스탬프를 보면 Runtime을 쓰는 테스트 셋이 모두 시작 후 15.4초에 함께 끝났고, 앱 코드는 0.28초에 끝난 직전 실행과 같았다. 첫 프레임이 러너에서 15초 걸려 5초짜리 메시지가 확인 전에 만료된 것이다. 메시지 시계를 첫 프레임 뒤에 시작하도록 바꿨다(`4834688`). 단언은 하나도 빼지 않았다. 첫 프레임이 왜 15초 걸렸는지(러너의 글꼴 캐시 등)는 확인하지 못했다.

## G. 성능과 메모리 측정

측정 환경: Mac mini, Apple M4 Pro, macOS 27.3, release 빌드. 사용자가 실행 중인 Aun과 설정을 건드리지 않도록 `HOME=<스크래치>`로 별도 인스턴스를 띄웠다(사용자 Aun이 47810/47811을 쓰고 있어 이 인스턴스는 임의 포트, 탐색 꺼짐).

### 메모리와 유휴 비용

| 상태 | phys_footprint | 유휴 CPU | 컨텍스트 전환/s | 비고 |
|---|---|---|---|---|
| 창 열림, 아무것도 안 함 | 28 MB | 0.1% 미만(30 s에 0.02 s) | 약 119 | |
| `--background`(창을 한 번도 안 염) | 14 MB | 0.1% 미만 | 약 49 | 로그인 시 실행 상태 |
| 클립보드 폴링만 돌리는 빈 프로세스 | | 0% | 약 5.8 | 잠만 자는 프로세스는 약 1 |
| 노드 둘(링크 없음) → 링크 하나 추가 | | 0% | 14.2 → 14.7 | 링크 하나 유지 비용 약 0.5/s |

- 창을 닫았을 때(`release_gpu_when_closed`)는 창을 연 뒤 닫는 조작이 필요한데, 접근성 권한이 없어 클릭할 수 없었다. 가장 가까운 상태로 `--background`(GPU를 만든 적 없음)를 쟀다.
- 앱 전체의 유휴 깨어남 중 Aun 코드(클립보드 폴링 약 5/s, `aun-keep` 0.2/s, 링크 핑 0.1/s)는 일부다. 나머지(창 열림 약 110/s, 백그라운드 약 43/s)는 메인 스레드(rune/AppKit) 쪽이다. 스레드별로 어디서 깨어나는지는 `sample`로는 알 수 없었고(대기 위치만 보인다), rune 내부 프로파일링은 이번 범위에서 하지 않았다. 다음에 볼 곳으로 남긴다.
- 클립보드 300 ms 폴링을 OS 알림으로 바꿀지: 폴링 단독 비용이 초당 약 5회 깨어남, CPU 0%로 측정됐다. 백그라운드 앱 전체(약 49/s)의 10% 정도다. macOS에는 변경 알림이 없어 폴링이 표준이고, Windows `AddClipboardFormatListener`는 메시지 전용 창과 메시지 루프 스레드가 하나 더 필요하다. 절약분(초당 약 4회)에 비해 이득이 작아 **바꾸지 않았다**. 더 줄이고 싶다면 더 간단한 방법은 연결된 기기가 없을 때 폴링 간격을 늘리는 것이다(구현하지 않음).

### 전송 속도: 병목과 개선

- 프로파일(`sample`, 심볼을 남긴 release 빌드, 512 MiB 루프백): 바쁜 샘플 중 ChaCha20 454, Poly1305 252, `memmove` 29, 소켓 송수신 약 140. **암호화가 약 80%**였다. Apple Silicon에서 RustCrypto `chacha20` 0.9는 `--cfg chacha20_force_neon` 없이는 스칼라 구현을 쓰고, `poly1305` 0.8에는 ARM 가속이 없다. 청크 크기, `BufWriter` 크기, 버퍼 복사(`memmove` 3%)는 병목이 아니어서 바꾸지 않았다.
- 메모리 안에서 암호화+프레이밍만 잰 결과(`seal_open_speed`, 64 MiB, MB/s, 봉인/열기):

  | 링크 암호 | Mac 로컬(M4 Pro) | Mac CI 러너 | Windows CI 러너(x86) |
  |---|---|---|---|
  | ChaChaPoly, 스칼라(이전) | 535–641 / 632–643 | 340 / 327 | — |
  | **ChaChaPoly, NEON(채택)** / x86은 AVX2 | **666–885 / 888–889** | (다음 CI) | 1061 / 1172 |
  | AES-GCM, 플래그 없음(소프트웨어 AES) | 219 / 218 | | |
  | AES-GCM, ARMv8 AES / x86 AES-NI | 2330–2446 / 2188–2388 | 1395 / 1420 | 942 / 1034 |

- **AES-GCM으로 바꿨다가 되돌렸다**(`98e70c7` → `b39fee2`). Mac에서는 3.5배 빨랐지만:
  1. Windows x86 러너에서는 AES-GCM이 ChaChaPoly보다 약 10% 느렸다(위 표). 두 기기 중 배터리가 있는 쪽은 Windows 노트북이고, 그쪽 CPU가 늘어난다(러너 수치, 노트북 CPU는 미확인).
  2. 실제 두 기기는 1 GbE 또는 Wi-Fi(상한 약 110 MB/s 이하)로 연결되어 두 암호 모두 네트워크보다 빠르다. Mac의 이득은 속도로 나타나지 않고, 전원에 연결된 데스크톱의 CPU만 줄인다.
  3. 실패했을 때의 손해가 다르다. Mac 빌드 플래그가 빠지면(예: 환경 변수 `RUSTFLAGS`) AES-GCM은 219 MB/s로 지금보다 2.5배 느려진다. ChaChaPoly는 플래그가 빠져도 이전 속도일 뿐이다.
- 채택: 선(wire)은 ChaChaPoly 그대로, Apple Silicon 빌드는 `.cargo/config.toml`의 `[target.aarch64-apple-darwin] rustflags`에 `--cfg chacha20_force_neon`(그리고 측정용 AES 플래그)을 넣어 ChaCha20을 NEON으로 돌린다. 새 의존성 없음. 환경 변수 없이 일반 `target/`에서 측정해 플래그가 적용된 것을 확인했다(표의 채택 줄).
- 루프백 512 MiB(디스크 쓰기 포함): 디스크 상태에 따라 밤사이 절대값이 크게 움직여(이른 시각 이전 빌드 461–499 MB/s, 늦은 시각 251–282 MB/s), 같은 시각에 번갈아 쟀다. **NEON 301–308 MB/s, 이전 251–282 MB/s(약 +10–20%)**. 100 MB당 봉인 CPU는 Mac에서 약 0.16–0.19 s → 약 0.11–0.15 s.
- 상호 운용: Mac(NEON)과 PC(AVX2)가 같은 바이트를 내는지 `this_machine_s_chacha_poly_matches_the_standard`가 확인한다. RFC 8439 §2.8.2 벡터(chacha20poly1305 크레이트 테스트에서 가져옴)와 비교하고, snow의 ChaChaPoly nonce 배치(0 4바이트 + 카운터 little-endian)도 맞춘다. 두 OS의 CI에서 돈다.
- 실제 노트북에서 판단하려면: `cargo test --release -p aun-core --lib -- --ignored --nocapture seal_open_speed`가 두 암호를 모두 잰다. 노트북에서 AES-GCM이 확실히 빠르면, `wire::NOISE` 한 줄과 알려진 답 테스트만 바꾸면 된다(양쪽 기기를 함께 업데이트).
- 호환성: 암호는 그대로지만 작업 D(`ClipImage`)와 E(`FileCancel`)의 새 프레임을 이전 빌드가 받으면 `frame kind` 오류로 링크를 끊는다. **두 기기를 함께 업데이트해야 한다.** 페어링은 그대로 유효하다.
- 파일마다 `sync_all`: 이 Mac에서 1 MiB 파일 80개가 페어링 포함 0.70–0.82 s(파일당 10 ms 미만)였다. Windows CI 러너에서는 파일당 약 0.5 s가 걸렸다(80개 테스트가 30 s에 55개). 러너 디스크 한 대의 숫자라 충돌 안전성(이름 바꾸기 전 동기화)을 빼지 않았다. 실제 PC에서 파일 여러 개를 받을 때 느리면 이것을 먼저 의심한다.
- 클립보드 이미지 인코딩(작업 D): 1920×1080 실제 화면 캡처를 9.6 ms에 802 KB PNG로, 디코딩 11.7 ms.

### 측정 도구로 남긴 것

- `cargo test --release -p aun-core --lib -- --ignored --nocapture seal_open_speed`: 두 암호의 메모리 내 속도(CI에서도 두 OS 모두 실행).
- `cargo test --release -p aun-core --test link -- --ignored --nocapture throughput`: 루프백 512 MiB.
- `... --ignored --exact clip::tests::poll_alone` / `sleep_alone`, `... --test link -- --ignored --exact idle_nodes` / `idle_link`: `top -l 2 -s 30 -pid <pid> -stats pid,cpu,csw,idlew`로 보는 유휴 비용.
- `AUN_SHOT=<png> cargo test --release -p aun-core --lib -- --ignored --nocapture png_speed`: 실제 스크린숏으로 PNG 속도.

### 확인하지 못한 것과 해 볼 것

- Windows 실기기의 메모리와 유휴 비용, 실제 전송 속도(작업 관리자 › 세부 정보의 메모리, `aun.log`의 링크 시간).
- 두 기기 사이 실제 파일 전송에서 CPU 사용률이 이전보다 낮은지(Mac: 활동 모니터의 Aun CPU %, 같은 큰 파일).

## H. 마우스·키보드 공유(KVM) 설계

- 바뀐 것: `docs/kvm-design.md`(코드 없음). 화면 가장자리 판정(화면 합집합의 바깥 경계, 받는 쪽이 돌아오기를 판정), macOS 캡처(`CGEventTap`, 접근성·입력 모니터링 권한, 탭 타임아웃 재활성화, 보안 입력)와 주입(`CGEventPost`, 델타 필드, 클릭 횟수), Windows 캡처(`WH_MOUSE_LL`/`WH_KEYBOARD_LL`, 주입 표식으로 자기 입력 거르기, 훅 시간 제한, 보안 데스크톱)와 주입(`SendInput`, 스캔 코드), 선에는 USB HID 사용 코드, 프레임 목록(1단계는 기존 TCP 링크, 쓰기 큐에 입력 우선순위), 2단계 UDP 경로(누적 이동량, 번호 붙인 이벤트), 지연 목표 표, 입력을 돌려주는 안전장치 7가지, 단계별 계획(1단계는 주입만 하는 "원격 트랙패드"), 테스트 계획.
- TwinDesk 문서(`docs/protocol.md`, `docs/full-kvm.md`)는 GitHub API로 읽고 개념만 참고했다. 코드는 보지도 가져오지도 않았다.
- 확인하지 못한 것: 문서의 모든 OS 동작(권한 흐름, 타임아웃 값, 커서 분리 복구)은 실기기 확인 대상으로 표시했다.
