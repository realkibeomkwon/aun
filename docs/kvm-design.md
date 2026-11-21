# 마우스·키보드 공유(KVM) 설계

작성: 2026-11-21 야간 배치. 구현하지 않은 설계다. 입력을 가로채는 코드는 실기기에서 단계마다 확인하면서 넣는다.

목표: Mac과 Windows PC를 나란히 놓고, 한쪽의 마우스와 키보드로 화면 가장자리를 넘어 다른 쪽을 조작한다. 기존 링크(Noise XX, TCP 47810)와 페어링을 그대로 쓴다. TwinDesk의 `docs/protocol.md`(tdv1/tdv2)와 `docs/full-kvm.md`에서 개념(누적 이동량, 번호 붙인 이벤트, 별도 UDP 경로)만 참고했고 코드는 가져오지 않는다(GPL-3.0).

## 용어

- **주는 쪽(driver)**: 사용자가 실제로 만지는 마우스·키보드가 달린 기기. 입력을 가로채 보낸다.
- **받는 쪽(driven)**: 입력을 주입받는 기기.
- **건너감(crossing)**: 포인터가 경계를 넘어 다른 기기로 넘어간 상태. 건너갈 때마다 번호(`route`)가 바뀐다.

## 1. 화면 배치와 가장자리 판정

- 배치는 기기마다 하나의 "이웃 방향"으로 시작한다: 상대가 이 기기의 왼쪽/오른쪽/위/아래 중 어디에 있는지. 설정에 저장하고 상대에게도 반대 방향으로 알린다(`KvmLayout` 프레임). 여러 모니터 배치(겹친 좌표계)는 2단계.
- 화면 목록: macOS `CGGetActiveDisplayList` + `CGDisplayBounds`(전역 좌표, 주 화면 원점), Windows `EnumDisplayMonitors` + `GetMonitorInfoW`(가상 화면 좌표). 해상도·배치가 바뀌면(`CGDisplayRegisterReconfigurationCallback`, `WM_DISPLAYCHANGE`) 다시 읽는다.
- 가장자리 판정은 모든 화면의 합집합 경계에서 한다. 이웃 방향 쪽 바깥 경계에 포인터가 닿고, 그 방향으로 계속 밀 때(누적 2 px 이상) 건너간다. 화면 사이의 안쪽 경계는 건너감이 아니다.
- 실수 방지(선택): 가장자리에 250 ms 머물러야 건너가기, 또는 수정 키를 누른 채일 때만 건너가기. 기본은 즉시.
- 들어가는 위치: 나간 지점의 상대 높이(0~1)를 받는 쪽 화면 높이에 비례해 옮긴다(왼쪽/오른쪽 이웃일 때). 받는 쪽 반대 가장자리 1 px 안쪽에서 시작한다.
- 돌아오기: 받는 쪽은 주입한 포인터가 돌아오는 방향 가장자리에 닿으면 주는 쪽에 `KvmReturn`을 보낸다. 주는 쪽이 판정하지 않는 이유는 받는 쪽의 화면 배치를 주는 쪽이 정확히 알 수 없기 때문이다.

## 2. macOS

### 캡처(주는 쪽)

- `CGEventTapCreate(kCGSessionEventTap, kCGHeadInsertEventTap, kCGEventTapOptionDefault, mask, …)`. 마스크: 마우스 이동/드래그, 버튼 down/up, 스크롤, 키 down/up, `flagsChanged`.
- 건너간 동안에만 이벤트를 삼킨다(콜백이 NULL 반환). 평소에는 이동 이벤트만 보고 그대로 통과시킨다(가장자리 판정용). 건너가 있지 않을 때 키 이벤트는 탭 마스크에서 뺀다. 타이핑 지연을 만들 이유가 없다.
- 건너간 동안 포인터는 화면 가장자리에 고정한다: `CGAssociateMouseAndMouseCursorPosition(false)`로 커서를 떼고 `CGDisplayHideCursor`. 이동량은 이벤트의 `kCGMouseEventDeltaX/Y`로 읽는다(절대 위치는 고정이므로).
- 권한: 접근성(`AXIsProcessTrustedWithOptions`로 확인·요청). 키 입력 감시는 macOS 10.15+에서 "입력 모니터링" 권한도 필요하다(`IOHIDRequestAccess(kIOHIDRequestTypeListenEvent)`). 권한이 없으면 KVM 토글을 끄고 설정 화면으로 안내한다.
- 탭이 꺼지는 경우: 콜백이 늦으면 시스템이 `kCGEventTapDisabledByTimeout`으로 탭을 끈다. 콜백은 큐에 넣기만 하고 즉시 반환해야 하며, 이 이벤트를 받으면 `CGEventTapEnable(tap, true)`로 다시 켠다. 탭 스레드는 자기 `CFRunLoop`를 돌리는 전용 스레드.
- 보안 입력: 비밀번호 칸 등에서 `IsSecureEventInputEnabled()`가 참이면 키 이벤트가 탭에 오지 않는다. 이때 건너가 있으면 주는 쪽으로 강제 복귀하고 상태 줄에 이유를 보인다.

### 주입(받는 쪽)

- `CGEventCreateMouseEvent` / `CGEventCreateKeyboardEvent` / `CGEventCreateScrollWheelEvent2` → `CGEventPost(kCGHIDEventTap, …)`.
- 이동은 절대 위치로 주입하되 `kCGMouseEventDeltaX/Y` 필드도 채운다(게임, 3D 앱은 델타만 읽는다). 버튼을 누른 채 이동은 `kCGEventLeftMouseDragged`.
- 수정 키 상태는 `CGEventSetFlags`로 이벤트마다 붙인다. 따로 누른 수정 키는 `kCGEventFlagsChanged`.
- 더블클릭: `kCGMouseEventClickState`를 직접 센다(주는 쪽 OS의 클릭 횟수를 그대로 전달).
- 권한: 주입에도 접근성 권한이 필요하다.

## 3. Windows

### 캡처(주는 쪽)

- `SetWindowsHookExW(WH_MOUSE_LL)`, `SetWindowsHookExW(WH_KEYBOARD_LL)`, 메시지 루프를 도는 전용 스레드에서. 건너간 동안 훅이 1을 반환해 이벤트를 삼킨다.
- 이동량: 저수준 훅의 `MSLLHOOKSTRUCT.pt`는 가속이 적용된 절대 좌표다. 건너간 동안은 커서를 가장자리에 `SetCursorPos`로 되돌리고 그 차이를 이동량으로 쓴다. 더 정확한 방법은 Raw Input(`RegisterRawInputDevices`, `RIDEV_INPUTSINK`)의 상대 이동량이다. 1단계는 훅, 2단계에서 Raw Input.
- 주입한 이벤트가 다시 훅에 걸리지 않게 `LLMHF_INJECTED` / `LLKHF_INJECTED`를 거른다. 단 다른 프로그램(원격 데스크톱 등)이 주입한 입력은 통과시켜야 하므로, 주입할 때 `dwExtraInfo`에 Aun 표식(상수)을 넣고 그것만 거른다.
- 훅 시간 제한: 콜백이 레지스트리 `LowLevelHooksTimeout`(값은 Windows 버전마다 다르다, 실기기에서 확인)을 넘기면 Windows가 훅을 조용히 뗀다. 콜백은 큐에 넣고 바로 반환한다. 주기적으로(1 s) 훅이 살아 있는지 확인하는 방법은 공식적으로 없으므로, 건너간 동안 이벤트가 2 s 이상 안 오면 훅을 다시 건다.
- 보안 데스크톱(UAC, 잠금 화면, Ctrl+Alt+Del)과 관리자 권한 창: 사용자 권한 프로세스는 입력을 보지도 주입하지도 못한다(UIPI). 1단계 범위 밖. 그 상태를 감지하면(`OpenInputDesktop` 실패 또는 이름이 `Default`가 아님) 강제 복귀한다. TwinDesk는 이것을 서비스(LocalSystem)와 에이전트로 푼다. Aun은 필요해지면 별도 설계.

### 주입(받는 쪽)

- `SendInput`. 이동은 `MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`(0~65535 정규화 좌표), 휠은 `MOUSEEVENTF_WHEEL` / `MOUSEEVENTF_HWHEEL`(120 단위).
- 키는 스캔 코드로 주입한다(`KEYEVENTF_SCANCODE`, 확장 키는 `KEYEVENTF_EXTENDEDKEY`). 가상 키 코드로 주입하면 받는 쪽 키보드 배열에 따라 다른 글자가 나온다.
- `dwExtraInfo` = Aun 표식(위 캡처 참고).

## 4. 키 코드 매핑

- 선(wire)에는 **USB HID 사용 코드**(Keyboard/Keypad page 0x07)를 싣는다. 물리 키 위치 기준이라 배열(한글 2벌식, 영문, 일본어)과 무관하다. 문자를 보내지 않는다. 받는 쪽 OS의 입력기가 글자를 만든다.
- macOS: `kCGKeyboardEventKeycode`(가상 키 코드, ANSI 위치 기준) ↔ HID 표. Windows: 스캔 코드(세트 1, 확장 플래그 포함) ↔ HID 표. 두 표는 코드에 상수 배열로 둔다(약 110개 키). 공개 표(USB HID Usage Tables 1.4, Apple `Events.h`, Microsoft "Keyboard scan codes")에서 만든다.
- 수정 키 대응: 기본은 **물리 위치 그대로** 보낸다(Mac Command는 Windows 키, Option은 Alt, Control은 Ctrl). "Mac 단축키 습관 유지" 옵션을 켜면 Mac에서 Windows로 갈 때 Command와 Control을 맞바꿔 ⌘C가 Ctrl+C가 되게 한다.
- 한/영 키, 한자 키: HID `Lang1`(0x90), `Lang2`(0x91). macOS에서는 `kVK_JIS_Kana`/`Eisu`가 아니라 입력 소스 전환이므로, Mac이 받는 쪽일 때 `Lang1`은 입력 소스 전환 단축키(기본 Ctrl+Space 또는 Caps Lock)로 바꿔 주입하는 옵션이 필요하다. 실기기 확인 필수.
- 눌린 키 집합을 양쪽이 기억한다. 건너감이 끝나거나 링크가 끊기면 받는 쪽은 눌린 키·버튼을 모두 뗀다(아래 안전장치).

## 5. 프로토콜

### 경로

- **1단계: 기존 TCP 링크만.** 새 프레임 종류를 추가한다. 구현이 단순하고 순서가 보장된다. 단점은 Wi-Fi 패킷 손실 시 머리 막힘(head-of-line blocking): 손실 한 번에 그 뒤 이동이 20~300 ms 멈춘다. 파일 전송 중이면 이동 프레임이 파일 청크 뒤에서 기다린다. 그래서 KVM 프레임은 쓰기 큐에서 파일 데이터보다 먼저 나가야 한다(쓰기 스레드에 우선순위 큐 두 개: 제어/입력, 대량 데이터).
- **2단계: UDP 입력 경로.** 이동만 별도 UDP 소켓으로 보내고, 누적 이동량(running total)을 실어 손실을 재전송 없이 견딘다. 키·버튼은 UDP와 TCP 양쪽으로 보내고 번호로 한 번만 적용한다. 키는 TCP 링크 안에서 호스트가 만든 32바이트 난수를 `KvmLane` 프레임으로 보내 나눈다(Noise 링크 안이라 비밀이 지켜진다). 패킷은 AES-256-GCM(링크와 같은 하드웨어 경로), nonce = 송신 번호, 재전송 방지 창 64개.

### 프레임(1단계, 링크 프레임 종류 12~. 11은 `FileFrom`이 쓴다)

| 프레임 | 방향 | 내용 |
|---|---|---|
| `KvmLayout { side: u8, screens }` | 양쪽 | 이웃 방향, 화면 사각형 목록(설정 바뀔 때) |
| `KvmEnter { route: u32, y: f32, mods: u16 }` | 주는 → 받는 | 건너감 시작. 들어갈 상대 높이와 현재 수정 키 |
| `KvmMove { route: u32, mn: u32, cx: i32, cy: i32 }` | 주는 → 받는 | 건너간 뒤 누적 이동량(픽셀). 받는 쪽은 마지막으로 적용한 값과의 차이만 주입하고 `mn`이 오래된 것은 버린다 |
| `KvmButton { es: u32, button: u8, down: bool, clicks: u8 }` | 주는 → 받는 | 이벤트 번호 `es` |
| `KvmWheel { es: u32, dx: i16, dy: i16, precise: bool }` | 주는 → 받는 | 트랙패드(정밀)와 휠 노치 구분 |
| `KvmKey { es: u32, hid: u16, down: bool, mods: u16 }` | 주는 → 받는 | HID 사용 코드 |
| `KvmReturn { route: u32, y: f32 }` | 받는 → 주는 | 받는 쪽 가장자리에 닿음 |
| `KvmLeave { route: u32 }` | 주는 → 받는 | 건너감 끝(돌아옴, 취소, 안전장치). 받는 쪽은 눌린 것을 모두 뗀다 |

- 이벤트는 직전 이동의 `(mn, cx, cy)`를 함께 실어(fence) "클릭이 이동을 앞지르지 않게" 한다. TCP 1단계에서는 순서가 보장되므로 fence가 필요 없고, 2단계 UDP에서 필요하다.
- 이동 묶기: 주는 쪽은 이동을 2 ms 간격까지 모아 하나로 보낸다(최대 500 Hz). 1000 Hz 마우스도 프레임 수가 절반이다.
- 프레임 크기는 모두 20바이트 안팎이고, `SealWriter`가 큐가 빌 때 flush하므로 한 번에 하나의 Noise 메시지가 나간다. 이동 하나에 Noise 메시지 하나(헤더 2 + 태그 16 + 본문 약 20바이트).

## 6. 지연 목표

| 구간 | 목표(같은 LAN, 유선) | 측정 방법 |
|---|---|---|
| 캡처 콜백 → 큐 | < 0.2 ms | 콜백 안에서 `mach_absolute_time`/`QueryPerformanceCounter` |
| 큐 → 소켓 쓰기(암호화 포함) | < 0.3 ms | AES-GCM 20바이트 봉인은 수 µs |
| 네트워크 | 유선 0.3 ms, Wi-Fi 2~5 ms(손실 시 TCP는 수십~수백 ms) | 핑 프레임 왕복 |
| 받기 → 주입 | < 0.5 ms | 주입 직전 시각 |
| 합계(입력 → 받는 쪽 커서 이동) | 유선 < 2 ms, Wi-Fi p99 < 15 ms(2단계 UDP 이후) | 받는 쪽 로그의 시각 차(시계 동기화: 핑 왕복의 절반으로 오프셋 추정) |

- KVM 프레임은 파일 청크 뒤에서 기다리지 않아야 한다(위 우선순위 큐). 이것이 없으면 대용량 전송 중 지연이 수백 ms가 된다.
- 소켓: 입력 링크에 `TCP_NODELAY`(이미 켜져 있음). 2단계 UDP 소켓은 DSCP EF/음성 등급(`IP_TOS 0xB8`)으로 Wi-Fi WMM 음성 큐를 탄다. Windows는 `IP_TOS`를 무시하므로 qWAVE(`QOSAddSocketToFlow`, `QOSTrafficTypeVoice`)를 쓴다.

## 7. 안전장치: 입력을 반드시 돌려준다

입력을 삼키는 쪽에서 문제가 생기면 사용자가 자기 컴퓨터를 조작할 수 없게 된다. 모든 경로에서 **주는 쪽이 입력을 되찾는 것**이 기본값이다.

1. **링크 감시.** 건너간 동안 받는 쪽은 250 ms마다 `Ping`에 응답 시각을 담아 보낸다(평소 10 s보다 짧게). 주는 쪽은 500 ms 동안 아무것도 못 받으면 즉시 탭/훅의 삼키기를 멈추고 커서를 되돌린다. 링크 끊김(소켓 오류)도 같다.
2. **받는 쪽 정리.** `KvmLeave`를 받거나, 이동/이벤트 없이 링크가 끊기거나, 1 s 동안 주는 쪽 소식이 없으면 눌린 키와 버튼을 모두 뗀다(`key up`, `button up` 주입). 눌린 채 남은 Shift나 마우스 버튼이 가장 흔한 사고다.
3. **비상 탈출 키.** 주는 쪽에서 Ctrl+Alt+Shift+Esc(설정 가능)는 항상 탭/훅이 먼저 보고, 건너가 있으면 즉시 돌아온다. 이 조합은 받는 쪽으로 보내지 않는다.
4. **스레드 독립.** 캡처 콜백은 큐에 넣기만 한다. UI 스레드나 링크 쓰기가 막혀도 콜백은 바로 반환하고, 큐가 가득 차면(예: 1024개) 건너감을 끝낸다. 콜백이 늦어 OS가 탭/훅을 떼면(macOS 타임아웃 이벤트, Windows 무소식) 건너감을 끝내고 다시 건다.
5. **패닉과 종료.** 프로세스가 죽으면 OS가 탭/훅을 같이 정리하므로 입력은 돌아온다. 단 macOS의 `CGAssociateMouseAndMouseCursorPosition(false)`와 숨긴 커서는 프로세스 종료 시 복구되는지 실기기로 확인해야 한다. 패닉 훅과 `on_quit`에서 먼저 되돌린다.
6. **권한·보안 입력.** 접근성 권한이 중간에 회수되거나(macOS) 보안 데스크톱으로 바뀌면(Windows) 건너감을 끝낸다.
7. **기본 꺼짐.** KVM은 설정에서 켜야 동작하고, 켜는 순간 탈출 키를 화면에 보여 준다.

## 8. 단계

진행 상황(2026-11-21): 1단계 중 마우스(이동, 버튼, 휠)를 구현했다. 키 입력은 다음 커밋이다. 실제 프레임은 위 표와 다르다.

- `Hello` 끝에 KVM 버전 바이트(`wire::KVM`, 지금 1)를 덧붙였다. 양쪽 버전이 같을 때만 `Kvm*` 프레임을 보낸다. 옛 빌드는 모르는 프레임을 받으면 링크를 끊기 때문이다.
- `KvmMove { route, mn, cx, cy }`(12), `KvmButton { button, down, clicks }`(13), `KvmWheel { dx, dy }`(14, 픽셀), `KvmLeave`(15). `KvmEnter`, `KvmReturn`, 이벤트 번호 `es`는 2단계에서 넣고 그때 버전을 올린다.
- 받는 쪽은 설정 "원격 조작 허용"(기본 꺼짐)이 켜졌을 때만 주입한다. 꺼져 있으면 프레임을 버리고 눌린 버튼을 뗀다.
- 링크마다 `kvm::Injector`가 눌린 버튼을 기억하고, `KvmLeave`, 링크 끊김, 허용 꺼짐에서 모두 뗀다.
- 보내는 쪽은 Aun 창의 "원격 조작" 패드다. 이동은 점 하나에 2 px(`PAD_GAIN`)이다. 이동과 휠은 링크 큐가 차면 버리고(누적값이라 거리는 잃지 않는다), 버튼과 `KvmLeave`는 250 ms까지 자리를 기다린다.
- 한계: 입력 프레임은 아직 파일 청크 뒤에서 기다린다(우선순위 큐는 2단계). macOS는 접근성 권한이 없으면 주입한 입력을 조용히 버리므로, 허용을 켤 때 권한을 요청하고 첫 입력 때 권한 상태를 로그에 남긴다.

1. **받는 쪽 주입만**(위험 낮음): 상대가 보낸 이동/클릭/키를 주입. 테스트용으로 Aun 창 안의 "원격 트랙패드" 영역에서 드래그하면 상대 커서가 움직이게 한다. 입력을 삼키지 않으므로 안전하게 지연과 키 매핑을 실기기에서 맞춘다.
2. **가장자리 건너감, TCP 1단계**: 캡처, 삼키기, 안전장치 1~5. 쓰기 큐 우선순위.
3. **키 매핑 다듬기**: 한/영, 수정 키 옵션, 미디어 키.
4. **UDP 입력 경로**(2단계): 누적 이동량, 번호 붙인 이벤트, 재전송 방지, DSCP.
5. **여러 모니터 배치 편집 화면**.

## 9. 테스트

- 단위: HID ↔ macOS 키 코드 / Windows 스캔 코드 표 왕복(모든 키), 가장자리 판정(화면 합집합, 안쪽 경계 무시), 누적 이동량 적용(중복·순서 바뀜·손실), 이벤트 번호 중복 제거.
- 통합(루프백, 주입은 가짜 싱크): 두 노드 사이 건너감 → 이동 → 클릭 → 돌아옴, 링크를 끊었을 때 받는 쪽이 눌린 키를 모두 떼는지, 주는 쪽이 500 ms 안에 돌아오는지.
- 실기기만 가능한 것: 실제 탭/훅, 권한 흐름, 보안 입력, 잠자기/깨우기 중 건너가 있던 경우, 입력기 전환, 지연 측정.
