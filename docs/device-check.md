# 실기기 점검 결과

`docs/next-tasks.md` 1번 작업. 기기: Mac(Kibeoms-Mac-mini, 192.168.0.4), Windows 노트북(KIBEOMKWON, 192.168.0.15).
점검 시작: 2026-11-21. Mac 빌드: `main` 34c56dc와 같은 코드(`overnight-a68e04` release).

| 항목 | 결과 | 근거 |
|---|---|---|
| 비밀번호 클립보드 제외 | 통과(수정 후) | Mac Chrome 비밀번호 관리자 → Windows: 안 감. Windows Chrome → Mac: 안 감. 일반 텍스트 Windows → Mac: 감. Mac Apple 암호 앱 → Windows: **비밀번호가 감**. 원인: 형식만 기록한 감시에서 Apple 암호 앱의 복사는 `public.utf8-plain-text` 하나뿐이고, Mac Chrome 비밀번호 관리자의 복사는 `org.nspasteboard.ConcealedType`을 함께 넣었다(2026-11-21 00:21 UTC). Apple 암호 앱은 비밀번호 표시를 넣지 않으므로 클립보드 형식으로는 구분할 수 없다. 수정: 변경을 알아챈 순간 맨 앞의 앱이 Apple 암호 앱(`com.apple.Passwords`, `.MenuBarExtra`)이면 비밀번호로 본다. 사용자 요청으로 "비밀번호 공유" 설정을 추가했다(기본 꺼짐, 켜면 표시된 복사도 보낸다). 재시험(새 빌드 9e85a42): Apple 암호 앱 창, 메뉴 막대 아이콘 모두 안 감, 일반 텍스트 감, 설정을 켜면 암호 앱 복사도 감. 버튼이 설정 행에 잘리지 않고 보인다 |
| 전송 취소 | 통과 | 받는 쪽, 보내는 쪽 취소 모두 남은 파일 없음, 다음 파일 전송됨. **이어 보내기 이후(2026-11-21) 기준이 바뀌었다**: 보내는 쪽 취소와 연결 끊김은 받는 쪽에 `.part`를 남기고 "다시 보내기"가 거기서부터 보낸다. 받는 쪽 취소는 여전히 남기지 않는다. 실기기 재시험 통과(2026-11-21, CI 44891270710 빌드, Mac ↔ Windows 양방향): 보내는 쪽 취소, 잠자기로 끊김 모두 이어 보냈고 받은 파일이 원본과 같았다. 받는 쪽 취소는 `.part`를 남기지 않았다 |
| 여러 파일 전송 | 통과 | 모두 도착. 개선점: 전송 기록이 최대 6줄만 보이고 스크롤이 없음, 긴 파일 이름이 잘림 |
| 받은 파일 보기 | 통과 | 파일이 선택된 채로 열림. 개선점: Windows는 같은 폴더 창이 열려 있어도 매번 새 탐색기 창을 연다 |
| 저장 폴더 | 통과 | 바탕화면에 저장됨. 재시작 후에도 유지 |
| Windows 트레이 | 통과 | Aun 열기, 클립보드 공유 켜기/끄기, 종료 모두 동작 |
| 로그인 시 자동 실행 | 통과 | Mac, Windows 각각 재시작 후 로그인하자 Aun이 떴다(Mac: LaunchAgent로 `--background` 실행, PID 679). 상대 Aun이 뜬 직후 바로 재연결 |
| Mac 메뉴 막대 메뉴 | 통과 | 새 빌드(9e85a42)에서 Aun 열기(창 닫힘 포함), 클립보드 공유 켜기/끄기(창 버튼과 함께 바뀜), 종료 모두 동작. 종료 후 프로세스 없음 확인 |
| 절전 해제 재연결 | 통과(수정 후) | 사용자: 한쪽을 잠자기로 바꿔도 상대 쪽은 계속 "연결됨"으로 보인다. Mac 잠자기(09:43:21~09:44:15 KST): `pmset -g log`에 잠자기 7 s, 6 s, 14 s 사이로 DarkWake 17 s, 10 s가 끼어 있다. Mac 로그에 `woke after` 줄이 없고, 09:44:30 Windows가 읽기 시간 초과(DEAD 35 s, Windows 로그 `TimedOut ... os error 10060`)로 링크를 끊은 뒤 2.5 s 만에 `link 7 up`. 해석: Windows가 마지막 프레임을 받은 시각은 09:43:55쯤으로 DarkWake(09:43:51~09:44:01) 안이다. Aun은 DarkWake 중에도 핑을 보내므로 상대는 35 s 침묵을 보지 못하고 "연결됨"을 유지한다. 또 벽시계·단조 시계 비교(`slept`, 10 s 기준)가 DarkWake로 쪼개진 잠자기를 놓쳐 `woke after` 줄이 없고, 복구는 깨어난 지 17 s 뒤 상대의 시간 초과 덕분이었다(놓친 정확한 이유는 미확인). Windows 절전: 노트북은 S0 저전원 유휴(Modern Standby, 네트워크 연결됨)만 지원한다(`powercfg /a`). Windows 로그에는 절전 흔적이 없다(09:35~09:42의 `tray up` 세 줄은 앱 재시작). 수정: OS의 잠자기 알림(macOS `NSWorkspaceWillSleepNotification`/`DidWake`, Windows `PowerRegisterSuspendResumeNotification`)을 받아 잠들기 직전에 링크를 닫고, 깨어날 때까지 링크를 받지도 걸지도 않는다. 회귀 테스트 `a_sleeping_node_drops_its_links_until_it_wakes`(수정 전 실패 확인). 실기기 재시험(빌드 10479c3, 양쪽): **Mac 잠자기 통과**. 11:57:36 `going to sleep: closing links`와 함께 Windows가 즉시 오프라인으로 표시. DarkWake(11:58:02, 11:58:34) 동안 Windows의 재연결 시도는 `refused before it was up`으로 거절되어 오프라인 유지(Windows 창에는 "연결 중"과 오프라인이 번갈아 보인다). 11:59:06 `woke: links allowed again`, 첫 다이얼은 네트워크가 아직 올라오지 않아 시간 초과, 11:59:13 `link 4 ... up`(깨어난 지 7 s). **Windows 절전 미확인**: 덮개를 닫았지만(11:59:50~12:00:50) 노트북이 절전에 들어가지 않은 것으로 보인다. Windows 로그에 `going to sleep`이 없고, Mac 쪽 링크도 35 s 시간 초과 없이 유지되어 Windows Aun이 계속 핑을 보냈다. 원인: 충전기 연결 중에는 덮개를 닫아도 최신 대기 모드에 들어가지 않는다(이벤트 506 없음). 전원 버튼은 이 노트북에서 종료다(종료 시에도 Mac은 즉시 오프라인, 부팅 후 바로 재연결). **Windows 절전 통과**(충전기 분리 후 덮개): 이벤트 506 12:08:21(이유 Lid)과 같은 순간 Windows 로그 `going to sleep: closing links`, Mac 로그 12:08:22 `link 5 down`. 이벤트 507 12:09:20, Windows 12:09:19 `woke: links allowed again`, 첫 다이얼은 네트워크 미연결(os error 10051), 12:09:20 `link up`(깨어난 지 1 s). Modern Standby에서도 `PowerRegisterSuspendResumeNotification` 알림이 제때 온다 |
| 주소로 페어링 | 통과(수정 후) | 사용자 동의 후 양쪽에서 해제(12:11:36, Mac `peers.txt` 백업)하고 Windows에서 Mac의 Tailscale 주소 `100.98.146.73`을 입력해 페어링: Mac 로그 12:12:43 `link 9 to KIBEOMKWON at Some(100.119.83.98:...) up`. Windows Aun 재시작 후 12:13:08 재연결. 다만 이 재연결은 같은 LAN의 탐색으로 먼저 이루어져(192.168.0.15) 저장된 주소로 다시 거는 경로는 실기기에서 따로 보이지 않았다. 그 경로는 `a_device_paired_by_address_reconnects_without_discovery` 테스트가 다룬다. 실기기 확인: 노트북을 iPhone 테더링(Tailscale 유지)에 연결하고 Windows Aun을 재시작하자 **연결되지 않았다**. Mac에서 `100.119.83.98:47810`으로 TCP 연결은 되므로 경로는 정상이고, Mac 로그에 Windows가 건 연결이 없었다. 원인: Windows `peers.txt`에 주소 없이 이름만 저장되어 있었다. 페어링 때 Mac이 같은 LAN 탐색으로 "근처 기기"였기 때문에, 입력한 주소를 "탐색이 못 찾는 기기"에만 저장하는 조건(`!p.nearby`)에 걸려 버려졌다. 수정: 탐색이 알려 준 주소와 다른 주소로 페어링하면 저장한다. 회귀 테스트 `a_typed_address_is_kept_even_when_discovery_saw_the_device`(수정 전 같은 증상으로 실패 확인). 별도 관찰(원인 미확인): Windows 로그 12:24:18에 Windows가 자기 Tailscale 주소 `100.119.83.98:47810`으로 연결을 건 줄이 있다. 재시험(양쪽 빌드 3558aba): 집 Wi-Fi에서 해제 후 `100.98.146.73`으로 다시 페어링(Mac 로그 12:42:33), Windows `peers.txt`에 `100.98.146.73:47810` 저장 확인. iPhone 테더링으로 옮겨 Windows Aun을 재시작하자 Mac 로그 12:43:11 `link 5 to KIBEOMKWON at Some(100.119.83.98:...) up`. 같은 빌드에서 Windows Chrome 비밀번호는 Mac으로 가지 않고 일반 텍스트는 가며, Windows 창에 "비밀번호 공유" 버튼이 보인다(비밀번호 클립보드 제외의 Windows 재확인) |

## 요약

10개 항목 모두 통과. 그중 3개는 점검 중 결함을 찾아 고친 뒤 같은 시험으로 다시 확인했다.

| 결함 | 커밋 | 회귀 테스트 |
|---|---|---|
| Apple 암호 앱에서 복사한 비밀번호가 상대 기기로 감(앱이 비밀번호 표시를 넣지 않음). "비밀번호 공유" 설정도 추가 | 9e85a42 | `clip::tests::a_copy_made_in_apple_passwords_is_secret` |
| 잠든 기기가 상대에게 계속 "연결됨"으로 보이고, 깨어난 뒤 재연결이 상대의 35 s 시간 초과에 기댐 | 10479c3 | `a_sleeping_node_drops_its_links_until_it_wakes` |
| 같은 LAN에 있을 때 입력한 주소(Tailscale)로 페어링하면 주소가 저장되지 않아 LAN을 벗어나면 재연결 안 됨 | 3558aba | `a_typed_address_is_kept_even_when_discovery_saw_the_device` |

## 알려진 한계와 후속 후보

- 맨 앞 앱 판별은 클립보드 변경을 알아챈 순간(300 ms 폴링)의 앞 앱을 본다. 메뉴 막대 항목에서 다른 앱 위로 복사하는 비밀번호 관리자는 놓칠 수 있다(Apple 암호 앱의 메뉴 막대 아이콘은 시험에서 걸러졌다).
- 깨어남 알림을 놓치면 잠듦 상태가 남아 Aun을 다시 시작할 때까지 링크를 거절한다. 안전장치는 아직 없다.
- Windows가 자기 Tailscale 주소로 연결을 건 로그 한 줄(원인 미확인).

## 점검 뒤 반영한 사용성 개선 (모두 실기기 확인, `main` 1baf1fb)

| 요청 | 반영 | 커밋 |
|---|---|---|
| 전송 기록이 6줄까지만 보이고 긴 이름이 잘림 | 완료 100개 보관, 기록 영역만 따로 스크롤, 긴 이름 줄바꿈 | 0e130dc, d93fb0e |
| 기록의 가독성(파일, 방향, 상대) | 파일 이름을 첫 줄에, 둘째 줄에 `기기 · 크기 · 상태`. 원 안의 아이콘: 받음 초록 ↓, 보냄 파랑 ↑, 실패 빨강 ✕, 취소 회색 —, 전송 중에는 진행률만큼 채워지는 원 | 8628876 … a916ba7 |
| 보내는 쪽 취소가 받는 쪽에 "실패"로 보임 | `FileAbort`에 취소 여부 1바이트(옛 빌드와 호환) | 49d9245 |
| 취소·실패한 보냄 다시 보내기 | "다시 보내기" 버튼, 누르면 그 행을 새 전송으로 바꾼다 | e496bd6, 15bca85 |
| 같은 파일을 전송 중에 또 보냄 | 같은 기기로 대기·전송 중인 파일은 다시 넣지 않음. 안내 배너와 해당 행 1.5 s 강조 | 834fe42, d4f63ea, 1baf1fb |
| Windows "탐색기에서 보기"가 매번 새 창 | `SHOpenFolderAndSelectItems`로 열린 창 재사용 | 6ee31c7 |
| 주소 대신 기기 이름으로 페어링 | 이름(Tailscale MagicDNS 등)을 별도 스레드에서 IPv4로 조회 | 3cf61f7 |

## 현재 실행 중인 빌드 (2026-11-21 14:35)

- Mac: 워크트리 `device-check-05e141`의 release 빌드 1baf1fb(`main`과 같은 코드). LaunchAgent도 이 경로를 가리킨다. Aun은 실행할 때마다 LaunchAgent 경로를 자기 경로로 고치므로, 워크트리를 지우기 전에 `main` 체크아웃에서 `cargo build --release -p aun`으로 빌드해 한 번 실행한다.
- Windows: CI 아티팩트 e496bd6의 `aun.exe`. 그 뒤의 변경은 보내는 쪽 화면(다시 보내기 행 교체, 중복 방지, 배너)뿐이라 받기와 링크에는 차이가 없다.
- 페어링: Windows가 Mac의 Tailscale 주소 `100.98.146.73:47810`을 저장하고 있다.
