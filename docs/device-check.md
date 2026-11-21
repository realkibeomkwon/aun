# 실기기 점검 결과

`docs/next-tasks.md` 1번 작업. 기기: Mac(Kibeoms-Mac-mini, 192.168.0.4), Windows 노트북(KIBEOMKWON, 192.168.0.15).
점검 시작: 2026-11-21. Mac 빌드: `main` 34c56dc와 같은 코드(`overnight-a68e04` release).

| 항목 | 결과 | 근거 |
|---|---|---|
| 비밀번호 클립보드 제외 | 통과(수정 후) | Mac Chrome 비밀번호 관리자 → Windows: 안 감. Windows Chrome → Mac: 안 감. 일반 텍스트 Windows → Mac: 감. Mac Apple 암호 앱 → Windows: **비밀번호가 감**. 원인: 형식만 기록한 감시에서 Apple 암호 앱의 복사는 `public.utf8-plain-text` 하나뿐이고, Mac Chrome 비밀번호 관리자의 복사는 `org.nspasteboard.ConcealedType`을 함께 넣었다(2026-11-21 00:21 UTC). Apple 암호 앱은 비밀번호 표시를 넣지 않으므로 클립보드 형식으로는 구분할 수 없다. 수정: 변경을 알아챈 순간 맨 앞의 앱이 Apple 암호 앱(`com.apple.Passwords`, `.MenuBarExtra`)이면 비밀번호로 본다. 사용자 요청으로 "비밀번호 공유" 설정을 추가했다(기본 꺼짐, 켜면 표시된 복사도 보낸다). 재시험(새 빌드 9e85a42): Apple 암호 앱 창, 메뉴 막대 아이콘 모두 안 감, 일반 텍스트 감, 설정을 켜면 암호 앱 복사도 감. 버튼이 설정 행에 잘리지 않고 보인다 |
| 전송 취소 | 통과 | 받는 쪽, 보내는 쪽 취소 모두 남은 파일 없음, 다음 파일 전송됨 |
| 여러 파일 전송 | 통과 | 모두 도착. 개선점: 전송 기록이 최대 6줄만 보이고 스크롤이 없음, 긴 파일 이름이 잘림 |
| 받은 파일 보기 | 통과 | 파일이 선택된 채로 열림. 개선점: Windows는 같은 폴더 창이 열려 있어도 매번 새 탐색기 창을 연다 |
| 저장 폴더 | 통과 | 바탕화면에 저장됨. 재시작 후에도 유지 |
| Windows 트레이 | 통과 | Aun 열기, 클립보드 공유 켜기/끄기, 종료 모두 동작 |
| 로그인 시 자동 실행 | 통과 | Mac, Windows 각각 재시작 후 로그인하자 Aun이 떴다(Mac: LaunchAgent로 `--background` 실행, PID 679). 상대 Aun이 뜬 직후 바로 재연결 |
| Mac 메뉴 막대 메뉴 | 통과 | 새 빌드(9e85a42)에서 Aun 열기(창 닫힘 포함), 클립보드 공유 켜기/끄기(창 버튼과 함께 바뀜), 종료 모두 동작. 종료 후 프로세스 없음 확인 |
| 절전 해제 재연결 | 통과(수정 후) | 사용자: 한쪽을 잠자기로 바꿔도 상대 쪽은 계속 "연결됨"으로 보인다. Mac 잠자기(09:43:21~09:44:15 KST): `pmset -g log`에 잠자기 7 s, 6 s, 14 s 사이로 DarkWake 17 s, 10 s가 끼어 있다. Mac 로그에 `woke after` 줄이 없고, 09:44:30 Windows가 읽기 시간 초과(DEAD 35 s, Windows 로그 `TimedOut ... os error 10060`)로 링크를 끊은 뒤 2.5 s 만에 `link 7 up`. 해석: Windows가 마지막 프레임을 받은 시각은 09:43:55쯤으로 DarkWake(09:43:51~09:44:01) 안이다. Aun은 DarkWake 중에도 핑을 보내므로 상대는 35 s 침묵을 보지 못하고 "연결됨"을 유지한다. 또 벽시계·단조 시계 비교(`slept`, 10 s 기준)가 DarkWake로 쪼개진 잠자기를 놓쳐 `woke after` 줄이 없고, 복구는 깨어난 지 17 s 뒤 상대의 시간 초과 덕분이었다(놓친 정확한 이유는 미확인). Windows 절전: 노트북은 S0 저전원 유휴(Modern Standby, 네트워크 연결됨)만 지원한다(`powercfg /a`). Windows 로그에는 절전 흔적이 없다(09:35~09:42의 `tray up` 세 줄은 앱 재시작). 수정: OS의 잠자기 알림(macOS `NSWorkspaceWillSleepNotification`/`DidWake`, Windows `PowerRegisterSuspendResumeNotification`)을 받아 잠들기 직전에 링크를 닫고, 깨어날 때까지 링크를 받지도 걸지도 않는다. 회귀 테스트 `a_sleeping_node_drops_its_links_until_it_wakes`(수정 전 실패 확인). 실기기 재시험(빌드 10479c3, 양쪽): **Mac 잠자기 통과**. 11:57:36 `going to sleep: closing links`와 함께 Windows가 즉시 오프라인으로 표시. DarkWake(11:58:02, 11:58:34) 동안 Windows의 재연결 시도는 `refused before it was up`으로 거절되어 오프라인 유지(Windows 창에는 "연결 중"과 오프라인이 번갈아 보인다). 11:59:06 `woke: links allowed again`, 첫 다이얼은 네트워크가 아직 올라오지 않아 시간 초과, 11:59:13 `link 4 ... up`(깨어난 지 7 s). **Windows 절전 미확인**: 덮개를 닫았지만(11:59:50~12:00:50) 노트북이 절전에 들어가지 않은 것으로 보인다. Windows 로그에 `going to sleep`이 없고, Mac 쪽 링크도 35 s 시간 초과 없이 유지되어 Windows Aun이 계속 핑을 보냈다. 원인: 충전기 연결 중에는 덮개를 닫아도 최신 대기 모드에 들어가지 않는다(이벤트 506 없음). 전원 버튼은 이 노트북에서 종료다(종료 시에도 Mac은 즉시 오프라인, 부팅 후 바로 재연결). **Windows 절전 통과**(충전기 분리 후 덮개): 이벤트 506 12:08:21(이유 Lid)과 같은 순간 Windows 로그 `going to sleep: closing links`, Mac 로그 12:08:22 `link 5 down`. 이벤트 507 12:09:20, Windows 12:09:19 `woke: links allowed again`, 첫 다이얼은 네트워크 미연결(os error 10051), 12:09:20 `link up`(깨어난 지 1 s). Modern Standby에서도 `PowerRegisterSuspendResumeNotification` 알림이 제때 온다 |
| 주소로 페어링 | 미시험 | |
