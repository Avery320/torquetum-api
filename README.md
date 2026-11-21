# Torquetum API

繁體中文 · [English](README.en.md)

Torquetum 的本機 HTTP API 提供專案配對、狀態查詢和 Asterism 幾何交換。本儲存庫包含 V0.0 的 OpenAPI 契約、Geometry Collection 的 JSON Schema，以及 Torquetum 用來提供 API 的 Rust 與 TypeScript 實作。外部程式透過 HTTP 連線至執行中的 Torquetum 專案。 [OpenAPI 契約](contracts/openapi.yaml) · [Geometry Collection Schema](contracts/geometry-collection.schema.json) · [Grasshopper 外掛 Alidade](https://github.com/Avery320/alidade)

## 快速開始

在同一台電腦上啟動支援 V0.0 的 Torquetum Desktop 或本機 `torquetum-server`。若從 Torquetum 原始碼啟動，先安裝其相依套件，再執行 `pnpm --filter @torquetum/studio build` 和 `cargo run -p torquetum-server`，開啟伺服器印出的網址。此 API 最初從 Torquetum 提交 `942ddd33307966e8b6cbe1b2ad8ed5856c454feb` 匯出。

1. 在 Studio 開啟獨立 Project。在 Asterism 將 **Geometry Input** 直接接到 **Geometry Output**，啟用兩個節點並將 **ID** 都設為 `fixture`。
2. 在該專案的 **Project Settings → External Connections** 複製配對碼。
3. 在 clone 下來的 API 目錄執行以下 Python 範例，並在提示出現時貼上配對碼。 [Python 範例](examples/python/asterism-exchange-smoke.py)

   ```sh
   python3 examples/python/asterism-exchange-smoke.py fixture fixture
   ```

範例只使用 Python 標準函式庫。它會替換測試 Input 的整棵資料樹兩次，核對寫入回執與指定版本的讀回結果，並驗證重送及過期版本的處理。請使用專供驗收的 Input。

## 客戶端配對

配對碼以 `TORAPI.` 開頭，後面是用無填充 base64url 編碼的 UTF-8 JSON，包含 `endpoint` 和 `invite`。向該端點的 `POST /v0.0/pair` 送出含 `invite` 與 `clientName` 的 JSON，將回傳的 `accessToken` 用作 Bearer token。`GET /v0.0/project` 可查詢目前專案、權限、交換就緒狀態及傳輸上限。配對碼限用一次。配對碼到期或授權失效時，從專案設定複製新碼。

客戶端直接連到 `http://127.0.0.1:<port>`，並關閉重新導向、代理伺服器與 Cookie。宿主會檢查連線來源與 `Host`，並拒絕帶有瀏覽器 `Origin` 的原生 API 請求。草稿專案即可配對。交換幾何前，需在 Studio 開啟同一個專案。獨立專案可讀寫，LAN 共享專案在同步完成後可讀取。

## 發送與讀回幾何

1. 設定已啟用的 **Geometry Input** 和 **Geometry Output** 的 **ID**。ID 可用英文字母、數字、連字號或底線，區分大小寫，並在各自的輸入或輸出方向中保持唯一。
2. 用 `GET /v0.0/asterism/ports` 取得 Input 的強式 ETag。以 `PUT /v0.0/asterism/inputs/{id}` 送出完整資料樹，在 `If-Match` 填入 ETag，並在 `X-Tor-Request-Id` 填入新的 UUID。
3. 收到 `202` 後，使用同一份授權查詢 `Location` 指定的同宿主相對路徑 `GET /v0.0/requests/{requestId}`。回執狀態為 `pending`、`committed` 或 `rejected`。`committed` 的結果包含 Input 的 `id` 與新 `etag`。如果需要重試結果未明的寫入，請沿用相同的 UUID、路徑、標頭和本文位元組。
4. 使用 `GET /v0.0/asterism/outputs/{id}` 讀回結果。要讀指定寫入的結果，請在 `X-Tor-Request-Id` 填入該次 UUID。成功回應的 ETag 對應該 Input 版本。遇到 `OUTPUT_NOT_READY` 可稍後重試，`OUTPUT_UNAVAILABLE` 表示目前沒有可用幾何，被新版取代的 Input 版本會回傳 `INPUT_SUPERSEDED`。

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

單元測試使用樣本驗證 HTTP 宿主與工作階段。需要驗證完整 HTTP 收發時，請依快速開始連到實際運行的 Torquetum 專案。

## 授權

本專案以 MIT 授權發布。 [MIT 授權全文](LICENSE)
