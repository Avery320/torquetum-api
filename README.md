# Torquetum API

繁體中文 · [English](README.en.md)

Torquetum 的本機 HTTP API 提供專案配對、狀態查詢和 Asterism 幾何交換。本儲存庫包含 V0.0 的 OpenAPI 契約、Geometry Collection 的 JSON Schema，以及 Torquetum 用來提供 API 的 Rust 與 TypeScript 實作。外部程式透過 HTTP 連線至執行中的 Torquetum 專案。 [OpenAPI 契約](contracts/openapi.yaml) · [Geometry Collection Schema](contracts/geometry-collection.schema.json) · [Grasshopper 外掛 Alidade](https://github.com/Avery320/alidade)

外部程式使用自己的幾何來源和 HTTP 客戶端。將來源幾何轉成 Geometry Collection，並用配對取得的授權送到指定的 Asterism Input ID。Torquetum 會驗證、更新 Input、求解下游節點，並提供 Output 給外部程式讀取。下載此儲存庫可取得規格、JSON 樣本和可執行的 Python 範例。 [幾何格式](#幾何格式)

## API 一覽

| 方法 | 路徑 | 用途 |
| --- | --- | --- |
| `POST` | `/v0.0/pair` | 用一次性配對邀請取得專案的 `accessToken`。 |
| `GET` | `/v0.0/project` | 查詢專案身分、讀寫權限、交換就緒狀態與傳輸上限。 |
| `GET` | `/v0.0/asterism/ports` | 列出可交換資料的 Geometry Input 和 Geometry Output ID，以及各 Input 的 ETag。 |
| `PUT` | `/v0.0/asterism/inputs/{id}` | 替換指定 Input 的整棵幾何資料樹。受理後回傳回執位置。 |
| `GET` | `/v0.0/requests/{requestId}` | 使用原寫入授權，查詢回執為 `pending`、`committed` 或 `rejected`。 |
| `GET` | `/v0.0/asterism/outputs/{id}` | 讀取指定 Output 已求解的幾何資料樹。可帶 `X-Tor-Request-Id` 讀取特定寫入的結果。 |

## 快速開始

在與用戶端相同的電腦上，啟動支援 V0.0 的 Torquetum Desktop 或本機 Web（由 `torquetum-server` 提供）。

1. 在 Studio 開啟供驗收使用的獨立 Project。在 Asterism 將 **Geometry Input** 直接接到 **Geometry Output**，啟用兩個節點並將 **ID** 都設為 `fixture`。
2. 在該專案的 **Project Settings → External Connections**，點選 **Pairing code** 旁的 **Copy**。
3. 下載 API 儲存庫，執行 Python 範例，並在提示出現時貼上配對碼。 [Python 範例](examples/python/asterism-exchange-smoke.py)

   ```sh
   git clone https://github.com/Avery320/torquetum-api.git
   cd torquetum-api
   python3 examples/python/asterism-exchange-smoke.py fixture fixture
   ```

範例只使用 Python 3 標準函式庫。它會用同一個點的兩組座標更新測試 Input，核對指定版本的讀回結果，並驗證重送和過期版本。驗收成功時顯示 `Verified two revisions of the same Asterism input, exact output reads and idempotent retries.`

## 自行呼叫 HTTP

沿用快速開始的測試節點。下列命令使用 `curl` 與 Python 3，請在 API 儲存庫根目錄的同一個終端機中依序執行。

從同一個專案複製一組新的配對碼，將 `TORAPI.` 後面的內容以無填充 base64url 解碼為 UTF-8 JSON，再取出 `endpoint` 和 `invite`。所有請求使用該 `endpoint`，格式為 `http://127.0.0.1:<port>`。配對碼限用一次，配對碼到期或授權失效時，從專案設定複製新碼。 [配對碼解碼實作](examples/python/asterism-exchange-smoke.py)

1. **換取授權。** 用配對碼解出的實際值取代範例中的網址和 `<invite>`。從成功回應取出 `accessToken`。 [配對請求與回應樣本](fixtures/connection.json)

   ```sh
   tor_endpoint='http://127.0.0.1:44555'
   curl --noproxy '*' -sS "$tor_endpoint/v0.0/pair" \
     -H 'Content-Type: application/json' \
     --data '{"invite":"<invite>","clientName":"Geometry example"}'
   ```

2. **查專案狀態與 Input 版本。** 填入實際的 `accessToken`。以 `canWrite` 確認目前可以寫入，並從 `kinds` 和 `limits` 查詢支援的幾何類型與傳輸上限。在節點清單的 `items` 找到 `direction` 為 `input`、`id` 為 `fixture` 的項目，取出 `etag`。使用 JSON 解碼後的字串，保留 ETag 本身的雙引號。

   ```sh
   tor_token='<accessToken>'
   curl --noproxy '*' -sS "$tor_endpoint/v0.0/project" \
     -H "Authorization: Bearer $tor_token"
   curl --noproxy '*' -sS "$tor_endpoint/v0.0/asterism/ports" \
     -H "Authorization: Bearer $tor_token"
   ```

3. **送出混合幾何。** 將範例 ETag 換成查到的實際值。每筆新寫入使用新的 UUID。下列命令把儲存庫內的 JSON 直接送到 `fixture` Input，替換它的整棵資料樹。範例包含點、直線、折線、平面、方體和網格。 [基本 Collection 樣本](fixtures/geometry-collection-basic.json)

   ```sh
   tor_etag='"input-v1"'
   tor_request_id=$(python3 -c 'from uuid import uuid4; print(uuid4())')
   curl --noproxy '*' -sS -i -X PUT "$tor_endpoint/v0.0/asterism/inputs/fixture" \
     -H "Authorization: Bearer $tor_token" \
     -H 'Content-Type: application/json' \
     -H "If-Match: $tor_etag" \
     -H "X-Tor-Request-Id: $tor_request_id" \
     --data-binary @fixtures/geometry-collection-basic.json
   ```

4. **確認寫入。** `202 Accepted` 表示已受理。將 `tor_receipt` 設為回應標頭 `Location` 的完整相對路徑，使用原本的授權查回執。`pending` 時依 `Retry-After` 所示秒數再查。`committed` 表示 Input 已寫入，`result.etag` 是新版本。`rejected` 時查看 `result.error`，幾何驗證錯誤可能包含 `path`、`itemIndex` 和 `field`。

   ```sh
   tor_receipt='/v0.0/requests/<requestId>'
   curl --noproxy '*' -sS -i "$tor_endpoint$tor_receipt" \
     -H "Authorization: Bearer $tor_token"
   ```

5. **讀回這次結果。** 在已連到該 Input 的 Output 上讀取資料，帶上同一個 `X-Tor-Request-Id`。`409 OUTPUT_NOT_READY` 表示仍在求解，稍後再讀。`INPUT_SUPERSEDED` 表示該筆寫入已被新版取代。`OUTPUT_UNAVAILABLE` 表示目前沒有可用幾何。成功時回傳 Geometry Collection，回應的 `ETag` 和回執的 `result.etag` 一致。可在範例結果確認分支 `[0]` 的點為 `[1, 2, 3]` 公尺，分支 `[3, 0, 7]` 包含三角網格。

   ```sh
   curl --noproxy '*' -sS -i "$tor_endpoint/v0.0/asterism/outputs/fixture" \
     -H "Authorization: Bearer $tor_token" \
     -H "X-Tor-Request-Id: $tor_request_id"
   ```

將樣本 JSON 換成自己產生的 Collection，並依實際接線調整網址中的節點 ID。Input 和 Output 各用對應節點的 **ID**，支援英文字母、數字、連字號和底線，區分大小寫並在各自方向中保持唯一。要讀取最近完成的 Output，可省略 `X-Tor-Request-Id`。

寫入結果未明時，保留原本的授權、UUID、網址、標頭和 JSON 內容，只重跑步驟 3 的 `curl` 命令。收到 `412 STALE_REVISION` 時，先重新查 Input 的 ETag，再確認是否要用新 UUID 重新寫入。

原生客戶端需直接連線，並關閉重新導向、代理伺服器和 Cookie。宿主會檢查連線來源與 `Host`，並拒絕帶有瀏覽器 `Origin` 的原生 API 請求。草稿專案即可配對，需在 Studio 開啟同一個專案才可交換幾何。獨立專案可讀寫，LAN 共享專案在同步完成後可讀取。 [完整 HTTP 規格](contracts/openapi.yaml)

每次寫入會替換 Input 的完整資料樹，並排入 Asterism 求解。寫入驗證失敗時保留前一筆資料。清空或修改 Input ID 會清除資料。停用 Input 會暫停交換並更新 ETag，重新啟用同一個 Input 時會保留原有資料。關閉專案會釋放授權和記憶體中的交換資料。用 **Bake** 或 **Bake Geometry Tree** 將結果存成 Station 物件。

## 幾何格式

寫入 Input 與讀取 Output 使用相同的 Geometry Collection 格式。外層必填 `schemaVersion: "0.0"`、`units: "m"`、`space: "project"` 和 `branches`。座標使用右手系、Z 軸朝上的專案世界座標，座標與長度以公尺表示，數值須有限。

每個分支包含 `path` 和 `items`。`path` 由一個以上的非負整數組成，例如 `[0, 4]`，同一份 Collection 中不可重複。`items` 可混合多種幾何。讀回時各分支依 `path` 排序，保留分支內的項目順序、`null` 槽和空分支。每次寫入至少需包含一件幾何。

| 幾何 | JSON `kind` | 欄位與限制 |
| --- | --- | --- |
| 點（Point） | `point` | `position` 是 XYZ 座標 `[x, y, z]`。 |
| 直線（Curve） | `line` | `start` 和 `end` 是相異的 XYZ 座標。選填嚴格遞增的參數範圍 `domain: [t0, t1]`，預設為 `[0, 1]`。 |
| 折線（Curve） | `polyline` | `points` 為依序排列的 XYZ 座標，`closed` 為布林值。相鄰點須相異，開放折線至少兩點，閉合折線至少三個相異點且省略重複的閉合端點。選填嚴格遞增的 `domain: [t0, t1]`，預設從 0 到線段數。 |
| 平面（Plane） | `plane` | `origin` 為 XYZ 座標，`normal` 為法向量，`direction` 為 X 軸的參考方向。兩向量須非零且不平行。 |
| 方體（Box） | `box` | `plane` 包含 `origin`、`normal`、`direction`。在該平面所定的座標系下，以 `x`、`y`、`z` 各自填入嚴格遞增的 `[min, max]` 尺寸範圍。 |
| 網格（Mesh） | `triangle-mesh` | `positions` 至少有三個 XYZ 頂點，`triangles` 至少有一個三角面。每個面用三個從 0 起算的有效頂點索引，面積必須大於零。 |
| 曲面與 BRep | `brep-step` | `shapeType` 填單一面 `face`、殼 `shell` 或實體 `solid`。`step` 提供一個連通形狀的完整 STEP Part 21 文字。 |

- 曲線目前可交換直線與折線。獨立圓、圓弧及一般 NURBS 曲線尚無對應的交換型別。
- 網格傳輸頂點和三角面。來源有四邊面或多邊面時，先轉為三角面再填索引。
- BRep 接收端會核對 `shapeType` 與形狀相符，並使用 STEP 內宣告的單位。Torquetum 保留精確 CAD 形狀，再產生用於顯示的網格。

可沿用下列樣本建立自己的 Collection。Grasshopper 操作與 Rhino 幾何轉換請參照 Alidade 使用說明。 [基本 Collection](fixtures/geometry-collection-basic.json) · [混合 BRep Collection](fixtures/geometry-collection.json) · [Geometry Collection Schema](contracts/geometry-collection.schema.json) · [Alidade 使用說明](https://github.com/Avery320/alidade#readme)

## 專案結構

```text
contracts/      OpenAPI 與 Geometry Collection JSON Schema
fixtures/       配對和幾何資料樣本
examples/       Python 完整收發範例
rust/           Rust HTTP 宿主、內部橋接與測試
src/            TypeScript 橋接、配對與回執處理
tests/          TypeScript 工作階段測試和共用橋接樣本
```

## 開發與驗收

使用 Node.js 22.12 以上、pnpm 10，以及 Rust 1.91.1 以上。從儲存庫根目錄執行以下命令。

```sh
pnpm install --frozen-lockfile
pnpm typecheck
pnpm test
cargo test --manifest-path rust/Cargo.toml --locked
```

單元測試使用樣本驗證 HTTP 宿主與工作階段。需要驗證完整 HTTP 收發時，請依快速開始連到實際運行的 Torquetum 專案。此 API 最初從 Torquetum 提交 `942ddd33307966e8b6cbe1b2ad8ed5856c454feb` 匯出。

若有 Torquetum 主儲存庫的存取權，可依主專案說明初始化 submodule 與安裝依賴，再執行 `pnpm --filter @torquetum/studio build` 和 `cargo run -p torquetum-server` 啟動本機 Web。 [Torquetum 開發環境](https://github.com/Avery320/torquetum#開發環境)

## 授權

本專案以 MIT 授權發布。 [MIT 授權全文](LICENSE)
