# Torquetum API

繁體中文 · [English](README.en.md)

Torquetum 的本機 HTTP API 提供專案配對、狀態查詢和 Asterism 幾何交換。本儲存庫包含 V0.0 的 OpenAPI 契約、Geometry Collection 的 JSON Schema，以及 Torquetum 用來提供 API 的 Rust 與 TypeScript 實作。外部程式透過 HTTP 連線至執行中的 Torquetum 專案。 [OpenAPI 契約](contracts/openapi.yaml) · [Geometry Collection Schema](contracts/geometry-collection.schema.json) · [Grasshopper 外掛 Alidade](https://github.com/Avery320/alidade)

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

收發幾何需要在用戶端同一台電腦上執行支援 V0.0 的 Torquetum Desktop 或本機 `torquetum-server`。若有 Torquetum 主儲存庫的存取權，可依開發環境說明初始化 submodule 並安裝相依套件，再執行 `pnpm --filter @torquetum/studio build` 和 `cargo run -p torquetum-server`。用瀏覽器開啟伺服器印出的網址。 [Torquetum 開發環境](https://github.com/Avery320/torquetum#開發環境)

1. 在 Studio 開啟供驗收使用的獨立 Project。在 Asterism 將 **Geometry Input** 直接接到 **Geometry Output**，啟用兩個節點並將 **ID** 都設為 `fixture`。
2. 在該專案的 **Project Settings → External Connections** 複製配對碼。
3. 下載 API 儲存庫，執行 Python 範例，並在提示出現時貼上配對碼。 [Python 範例](examples/python/asterism-exchange-smoke.py)

   ```sh
   git clone https://github.com/Avery320/torquetum-api.git
   cd torquetum-api
   python3 examples/python/asterism-exchange-smoke.py fixture fixture
   ```

範例只使用 Python 3 標準函式庫。它會替換測試 Input 的資料樹兩次，核對指定版本的讀回結果，並驗證重送和過期版本。驗收成功時顯示 `Verified two revisions of the same Asterism input, exact output reads and idempotent retries.`

## 自行呼叫 HTTP

沿用快速開始的測試節點，並從同一個專案複製新的配對碼。配對碼的 `TORAPI.` 後面是無填充 base64url 編碼的 UTF-8 JSON。解碼後取出 `endpoint` 和 `invite`。`endpoint` 是此次連線的 `http://127.0.0.1:<port>`，所有請求都送到這個宿主。配對碼限用一次，配對碼到期或授權失效時，從專案設定複製新碼。

1. **換取授權。** 向 `POST /v0.0/pair` 送出 `{"invite":"<invite>","clientName":"Point example"}`，並設定 `Content-Type: application/json`。成功回應中的 `accessToken` 用於後續每個請求的 `Authorization: Bearer <accessToken>`。可用 `GET /v0.0/project` 查詢專案狀態、`canWrite` 和傳輸上限。 [配對請求與回應樣本](fixtures/connection.json)
2. **取得 Input 版本。** 呼叫 `GET /v0.0/asterism/ports`。在 `items` 中找到 `direction` 為 `input` 且 `id` 為 `fixture` 的項目，取出 `etag`。將解碼後的 `etag` 字串直接用於 `If-Match`，包含它的雙引號，例如 `"input-v1"`。
3. **發送一個點。** 用下列請求替換 `fixture` Input 的資料樹。填入實際的 port、token 和 ETag，每次新的寫入產生一個新的 UUID。此範例送出位於專案世界座標 `[1, 0, 0]` 公尺的點。

   ```http
   PUT /v0.0/asterism/inputs/fixture HTTP/1.1
   Host: 127.0.0.1:<port>
   Authorization: Bearer <accessToken>
   Content-Type: application/json
   If-Match: "input-v1"
   X-Tor-Request-Id: bd9499c5-0552-4309-87bf-cdbe9217d48e

   {
     "schemaVersion": "0.0",
     "units": "m",
     "space": "project",
     "branches": [{"path": [0], "items": [{"kind": "point", "position": [1, 0, 0]}]}]
   }
   ```

4. **確認寫入。** `202 Accepted` 表示寫入已受理。使用同一份授權，向回應的 `Location` 指定的同宿主相對路徑發送 `GET`。

   ```http
   HTTP/1.1 202 Accepted
   Content-Type: application/json
   Location: /v0.0/requests/bd9499c5-0552-4309-87bf-cdbe9217d48e
   Retry-After: 1

   {"requestId": "bd9499c5-0552-4309-87bf-cdbe9217d48e"}
   ```

   回執為 `pending` 時依 `Retry-After` 再查，`rejected` 時檢查 `result.error`。`committed` 表示 Input 已寫入，`result.etag` 是它的新版本。

5. **讀回這次寫入的結果。** 呼叫 `GET /v0.0/asterism/outputs/fixture` 並帶上同一個 `X-Tor-Request-Id`。Asterism 還在求解時會回傳 `409 OUTPUT_NOT_READY`，稍後重試讀取。成功時會收到同一個點的資料樹，回應的 `ETag` 和回執的 `result.etag` 一致。若該版本已被新版取代，會收到 `INPUT_SUPERSEDED`。`OUTPUT_UNAVAILABLE` 表示目前沒有可用幾何。

如果寫入的結果未明，重送時沿用相同的 UUID、路徑、標頭和本文位元組。Input 和 Output 的 ID 可各自設定，使用英文字母、數字、連字號或底線，區分大小寫並在各自方向中保持唯一。

原生客戶端需直接連線，並關閉重新導向、代理伺服器和 Cookie。宿主會檢查連線來源與 `Host`，並拒絕帶有瀏覽器 `Origin` 的原生 API 請求。草稿專案即可配對，需在 Studio 開啟同一個專案才可交換幾何。獨立專案可讀寫，LAN 共享專案在同步完成後可讀取。 [完整 HTTP 規格](contracts/openapi.yaml)

每次寫入會替換 Input 的完整資料樹，並排入 Asterism 求解。寫入驗證失敗時保留前一筆資料。清空或修改 Input ID 會清除資料。停用 Input 會暫停交換並更新 ETag，重新啟用同一個 Input 時會保留原有資料。關閉專案會釋放授權和記憶體中的交換資料。用 **Bake** 或 **Bake Geometry Tree** 將結果存成 Station 物件。

## 幾何格式

資料樹使用 `schemaVersion: "0.0"`，座標為專案世界座標，單位為公尺。傳輸保留分支路徑、項目順序、`null` 和空分支。單一 Collection 可以混合點、直線、多段線、平面、方體、三角網格，以及精確的 STEP 面、殼與實體。

每個 STEP 項目代表一個連通形狀。接收端會驗證宣告的幾何類型並使用 STEP 的宣告單位。Torquetum 保留精確幾何，再產生顯示用網格。遇到不支援的項目或無效尺寸時，可從錯誤資訊查出分支與項目位置。配對的專案會提供傳輸上限。

可使用隨附的合成樣本核對客戶端資料。Grasshopper 操作與 Rhino 幾何轉換請參照 Alidade 使用說明。 [配對樣本](fixtures/connection.json) · [基本 Collection](fixtures/geometry-collection-basic.json) · [混合 STEP Collection](fixtures/geometry-collection.json) · [Alidade 使用說明](https://github.com/Avery320/alidade#readme)

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

## 授權

本專案以 MIT 授權發布。 [MIT 授權全文](LICENSE)
