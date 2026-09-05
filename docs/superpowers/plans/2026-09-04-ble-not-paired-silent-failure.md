# BLE：设备被工厂复位后，主机全线静默失败却说一切正常

日期：2026-09-04
分支：`fix/ble-passive-reconnect-verify`
状态：根因已确认；主机侧缺陷已定位，待修

> 本文经过两次修正。初版给出的两个「机制」后来都被自己的实验证伪，第三版才拿到根因。
> 证伪过程保留在 §5 —— 那些错误的推断看起来都很像，但都不是，值得留着。

## 1. 现象

所有指纹认证静默失效（`sudo` / `pkexec` / `imk` 全部回退密码，
`AGENT_APPROVE rejected — device not verified`），**而所有状态展示都说一切正常**：
`Connected` / `Paired: Yes` / `Host 2 bound · active (this computer)`。同时**触摸解锁屏幕
仍然工作**（那条走 0x21 通知，不经认证握手），所以用户的感受是「指纹好着呢，就是今天
sudo 老要密码」。重启 daemon 能让现象消失一阵。

不是特权分离引入的：本次会话第一次出现是在旧的用户级 daemon 上。

## 2. 根因（已确认）

**用户对设备做了工厂复位。设备侧本机那个槽的配对数据被擦掉，而主机的 `pairing.json`
原封不动。** 固件的 pre-pair 白名单从此拒绝一切认证类命令，回 `0xF2`
（`SEC_ERR_NOT_PAIRED`）。`socket.rs:1030` 的注释描述过这类 split state。

证据是设备自己对 `SLOT_STATUS` 的回答，不经任何本地推断（固件布局
`[0x39][RSP_OK][bitmap][active_slot]`，`SLOT_1=1`/`SLOT_2=2`，bitmap 的 bit0/bit1 表示
两槽是否被占用）：

| 时刻 | 原始帧 | bitmap | active | 含义 |
|---|---|---|---|---|
| 故障时 | `[39 00 01 02]` | `0x01` 只有 slot 1 被占 | `2` | **当前生效的槽（2）在设备上是空的** |
| 恢复后 | `[39 00 03 02]` | `0x03` 两槽都被占 | `2` | 正常 |

设备还在每个会话的第一条 `GET_STATUS` 响应里直说了一遍：

```
[00 21 00 10 01 07 09 33 4a]
 │  │  │  │  └──┴──┴──┴── fw 1.7.9 build 0x334a
 │  │  │  └── 电量 0x10 = 16%
 │  │  └── paired = 0x00      ← 设备自己说：我没配对
 │  └── 指纹位图 0x21 = slot 0 与 slot 5
 └── 状态 OK
```

**两条被推翻的中间结论**（都曾被当成根因写进本文）：

- 「设备坐在另一个 host 槽上」——**错**。`active=2` 说明设备一直激活着本机这个槽，问题是
  这个槽的数据没了。区别关系到解法：切到别人的槽，切回来就好；自己的槽空了，只有重新
  配对能修。bitmap 从 `0x01` 变成 `0x03` 也印证 —— 单纯切换 host 不会让空槽变满。
- 「用户切 slot 5 再切回就恢复了」——**不能作为证据**。切换会强制断连重连，而重启 daemon
  同样能让现象消失一阵，两个解释混在一起。真正的证据只有上表那两帧。

## 3. 这为什么是个值得修的产品缺陷

工厂复位是文档里写明的用户操作。**任何人复位设备之后都会掉进这个状态**：认证全废，而每个
界面都说一切正常。而设备其实把真相说了两遍，主机两次都没听：

1. 每个会话第一条 `GET_STATUS` 响应里的 `paired=0x00`；
2. 之后每条认证命令回的 `0xF2`。

## 4. 主机侧缺陷

### 4.1 设备自报的 `paired` 位从未被使用

`ble.rs` 把这个字节解析进 `DeviceStatus.paired`，然后**全仓库没有任何地方读它**
（`grep -rn '\.paired'` 只剩 TUI 里另一个同名字段）。CLI 的 `Paired: Yes` 来自
`PAIR:STATUS`，那是按本地 `pairing.json` 推断的，不是设备的说法。`slot status` 的
「Host 2 bound · active (this computer)」同样是本地推断 —— 故障时它显示两个槽都 bound，
而设备说 bitmap=0x01。

**唯一知道真相的那个字节被丢掉了，所有 UI 在复述本地的一厢情愿。**

修法（按性价比排序）：

1. `STATUS` 响应带上设备自报的 paired 位；CLI/TUI 在「本机认为已配对、设备说没有」时显示
   明确的一行：`⚠ 设备已不再与本机配对（可能被工厂复位）—— 运行 immurok-cli pair 重新配对`。
2. daemon 侧：设备自报 `paired=0` 时不得把 `is_device_verified` 置 true，并通过 `NOTIFY:`
   通道推一条桌面通知 —— 这正是那条通道存在的意义。
3. `slot status` 的渲染改成按设备的回答，或至少标明哪些字段是本地推断。

### 4.2 「未知响应 → 假定已验证」的兼容分支在最该警惕时放行

`ble.rs:722-729`：拿到任何非 `[0x38][hmac]` 的帧都 `is_device_verified.store(true)`，
注释说是为兼容旧固件。本次事故里正是这条把「设备说我没配对」咽了下去，只留一行 WARN。

修法：只在按固件版本确认是旧固件时才走兼容路径；其余情况重试一次，仍不对判定失败。

## 5. 两个被证伪的机制（记录下来，免得再走一遍）

会话里观察到过一次真实的响应错位：

```
15:05:31.116  BLE TX: cmd=0x01 (GET_STATUS)
15:05:32.641  Challenge-Response: sending nonce      ← 1.52s 后，GET_STATUS 的响应还没到
15:05:32.641  BLE TX: cmd=0x38 (CHALLENGE)
15:05:32.759  BLE RX: [00210010010709334a]           ← 这是 GET_STATUS 的响应，迟到 1.64s
15:05:32.759  WARN Challenge-Response: unexpected response
```

**猜测一：`send_command_inner` 不按命令码过滤，通知被当成响应。证伪。**
`route_notification` 按帧形状分类（`[0x21][2B][8B]`=11 字节是指纹匹配、`[0x23]`=锁屏、
`[0x11]…`=录入进度…），不匹配已知形状的才交给 `pending_response`。按这个前提写的复现脚本
判据是「响应首字节 == 命令码」——**这个前提是编的**，结果 12/12 全部误报：被判成「顶包」
的 `[0021]` 恰恰是 FP_LIST 的正确响应（状态 00 + 位图 0x21）。脚本已删除。

**猜测二：并发命令抢占单个 `pending_response` 槽。证伪。**
`send_command_inner` 开头确实 `state.pending_response.take()` 丢弃上一个，看起来很可疑；
但 socket 来的命令全部经由 `ble_cmd_tx` 那条 mpsc 交给**单个** BLE worker，本就串行。
`repro-response-mixup.sh` 并发发命令 8 轮，daemon 一条 `response dropped` /
`unexpected response` / `timeout` 都没报。

**后续解开了**：GET_STATUS 根本没有「等待」——它的写直接失败了。daemon 在 helper
接上 D-Bus 之前就发命令（实测第一条 TX 比 helper 的 READY 早 49ms），`cmd_write` 立刻
返回 Err，而调用处是 `if let Ok(rsp)`，于是静默跳过；随后的挑战同样失败，却走进 Err
分支「assuming verified」。修复见「等 helper 就绪再发命令」那一版：READY 成为会话必须
等到的门，且传输层没通时不再自称已验证。

## 6. 关于「加一条错位诊断护栏」的修正

本文一度建议：响应投递给 `pending_response` 时，若首字节既不是在飞命令的命令码、也不是
已知通知形状，就打 ERROR。**这个建议基于一个错误前提，不要照做。**

固件的响应格式因命令而异（`firmware/APP/hidkbd.c`）：

| 命令 | 构造 | 首字节 |
|---|---|---|
| `FP_LIST` | `rspBuf[0]=IMMUROK_RSP_OK; rspBuf[1]=bitmap` | 状态 `0x00` |
| `SLOT_STATUS` | `rspBuf[0]=IMMUROK_CMD_SLOT_STATUS` | 回显命令码 |
| `CHALLENGE` | `rspBuf[0]=IMMUROK_CMD_CHALLENGE` | 回显命令码 |

所以「首字节 == 命令码」不是一条普适规则，任何以它为判据的检测器都会对 `FP_LIST` /
`GET_STATUS` 必然误报。`scripts/diag-device.sh` 里那段判读就是这么写的，在健康设备上
实测报「已确认异常」，已删除 —— 会喊狼来了的检测器比没有更糟。

真要做这条护栏，得逐命令记录期望的响应形状，或者让固件统一格式。在那之前，原始帧的
打印本身才是有用的东西。

## 7. 实机验证（2026-09-05，已通过）

设备当时两槽都占用（`[39 00 03 02]`：bitmap 0x03、active slot 2），所以
`unpair` 是可逆的。复现路径：

1. `pkexec cp -a /var/lib/immurok/pairing.json /var/lib/immurok/pairing.json.repro-bak`
2. `echo y | immurok-cli unpair` —— 设备清掉 slot 2，**并轮换该槽的 BLE 地址**
   （`2026-08-04-unpair-slot-address-rotation-design.md`），旧地址对象从 BlueZ 消失，
   设备以新地址 bond 上来
3. 把备份的 `pairing.json` 放回去 + 重启 daemon → 本地有配对、设备说没有 = split state

结果，四项预期全中：

```
Paired:     No (device says so)
⚠ The device is no longer paired with this computer.  … until you run: immurok-cli pair
ERROR Device reports it is NOT paired with this host … Refusing to mark the device verified.
Status:     Connected                       ← 会话没被断掉
slot status → Host 2 empty · active — the device is presenting this empty slot
```

`slot status` 能正常执行这一条尤其关键：它证明会话确实活着（第一版实现在这里
`return Ok(())` 断开了会话，那会让用户永远修不好，因为重新配对本身需要活的会话）。

恢复路径也走通了：`unpair`（设备回 0xF2 → 丢弃陈旧本地状态）→ `pair`（第二主机
流程：先指纹后按键）→ `Paired: Yes`，**警告立即消失**（配对成功即清标志，不等重连）
→ `imk run --agent -- sudo id -un` 返回 root。

顺带发现并修掉：split state 下 `pair` 会拿「Already paired. Unpair first」把用户挡回去，
而 `status` 上一秒才建议他跑 `pair`。既然设备已明说不认本机，本地那份就是废纸，
不该多绕一圈。

**这一处也已实机验证**（2026-09-05，第二轮）：重新造出 split state 后直接跑 `pair`，
输出第一行即新分支 —— `Local pairing exists but the device says it is not paired with
this computer — replacing the stale record.` —— 随后进入第二主机流程（指纹 + 按键）
并配对成功，没有被「Already paired. Unpair first」挡回。对照组：同一状态下已部署的
旧二进制仍然挡回。

第二轮还顺带确认了一件事：**每次清槽都会轮换该槽的 BLE 地址**。两轮下来设备用过
`0E:3D:5E:5C:C9:C6` → `D8:AF:B8:C1:80:DC` → `CB:4E:36:39:60:2E` 三个地址，BlueZ 里会
留下失效的设备对象，需要时用 `bluetoothctl remove <addr>` 清理。

**一个没验到的边界**：地址轮换意味着「设备清槽后仍以原地址连着本机」这种情况，
只在工厂复位（`slot_meta_reset` → 地址回出厂 MAC）时才出现。本次是靠新地址重新
bond 后再放回本地 pairing 来构造的，路径等价但不完全相同。

## 8. 复现与诊断工具

- **根因复现（确定性）**：对设备做工厂复位，但**不**在主机上 `unpair`。再连回本机即得：
  `Paired: Yes` + 认证全废 + 设备回 `0xF2`。
- `scripts/diag-device.sh` —— 不看本地推断，只看设备回的原始字节。
  （注：其中的「响应错位」判读沿用了 §5 猜测一的前提，**会误报**；看它打印的原始帧即可，
  那部分待 §6 的护栏落地后重写。）
- `scripts/repro-response-mixup.sh` —— 并发探针。本次结果为**阴性**，用于排除猜测二。
