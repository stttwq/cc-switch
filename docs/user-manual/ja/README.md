# CC Switch ユーザーマニュアル

> Claude Code / Codex / Pi オールインワンアシスタント

## 目次構成

```
CC Switch ユーザーマニュアル
│
├── 1. はじめに
│   ├── 1.1 ソフトウェア紹介
│   ├── 1.2 インストールガイド
│   ├── 1.3 インターフェース概要
│   ├── 1.4 クイックスタート
│   └── 1.5 個人設定
│
├── 2. プロバイダー管理
│   ├── 2.1 プロバイダーの追加
│   ├── 2.2 プロバイダーの切り替え
│   ├── 2.3 プロバイダーの編集
│   └── 2.4 並べ替えと複製
│
├── 3. 拡張機能
│   ├── 3.1 MCP サーバー管理
│   ├── 3.2 Prompts プロンプト管理
│   ├── 3.3 Skills スキル管理
│   └── 3.4 セッションマネージャー
│
└── 5. よくある質問
    ├── 5.1 設定ファイルの説明
    ├── 5.2 FAQ
    └── 5.4 環境変数の競合
```

## ファイル一覧

### 1. はじめに

| ファイル | 内容 |
|------|------|
| [1.1-introduction.md](./1-getting-started/1.1-introduction.md) | ソフトウェア紹介、主要機能、対応プラットフォーム |
| [1.2-installation.md](./1-getting-started/1.2-installation.md) | Windows インストールガイド |
| [1.3-interface.md](./1-getting-started/1.3-interface.md) | インターフェースレイアウト、ナビゲーションバー、プロバイダーカードの説明 |
| [1.4-quickstart.md](./1-getting-started/1.4-quickstart.md) | 5 分でできるクイックスタートチュートリアル |
| [1.5-settings.md](./1-getting-started/1.5-settings.md) | 言語、テーマ、ディレクトリ、クラウド同期の設定 |

### 2. プロバイダー管理

| ファイル | 内容 |
|------|------|
| [2.1-add.md](./2-providers/2.1-add.md) | プリセットの使用、カスタム設定 |
| [2.2-switch.md](./2-providers/2.2-switch.md) | メイン画面での切り替え、トレイでの切り替え、反映方法 |
| [2.3-edit.md](./2-providers/2.3-edit.md) | 設定の編集、API Key の変更、バックフィル機能 |
| [2.4-sort-duplicate.md](./2-providers/2.4-sort-duplicate.md) | ドラッグで並べ替え、プロバイダーの複製、削除 |

### 3. 拡張機能

| ファイル | 内容 |
|------|------|
| [3.1-mcp.md](./3-extensions/3.1-mcp.md) | MCP プロトコル、サーバーの追加、アプリバインド |
| [3.2-prompts.md](./3-extensions/3.2-prompts.md) | プリセットの作成、有効化の切り替え、スマートバックフィル |
| [3.3-skills.md](./3-extensions/3.3-skills.md) | スキルの発見、インストール・アンインストール、リポジトリ管理 |
| [3.4-sessions.md](./3-extensions/3.4-sessions.md) | セッションマネージャー：閲覧、検索、再開、削除 |

### 4. 認証情報と供給

API key は Windows 資格情報マネージャにのみ保存し、プロバイダー切り替え時にユーザーレベルの環境変数経由で CLI に供給します。SQLite、`settings.json`、live 設定ファイル、エクスポート、同期ペイロードにはいずれも鍵の値を残しません。ローカル HTTP プロキシと使用量ダッシュボードは削除されました。アプリ内の更新通知もなくなっており、新しいバージョンへのアップグレードは新しい Windows インストーラ（GitHub Releases）をダウンロードして上書きインストールします。

### 5. よくある質問

| ファイル | 内容 |
|------|------|
| [5.1-config-files.md](./5-faq/5.1-config-files.md) | CC Switch のストレージ、CLI 設定ファイル形式 |
| [5.2-questions.md](./5-faq/5.2-questions.md) | よくある質問と回答 |
| [5.4-env-conflict.md](./5-faq/5.4-env-conflict.md) | 環境変数の競合検出と対処 |

## クイックリンク

- **初めての方**：[1.1 ソフトウェア紹介](./1-getting-started/1.1-introduction.md) からお読みください
- **インストールの問題**：[1.2 インストールガイド](./1-getting-started/1.2-installation.md) をご確認ください
- **プロバイダーの設定**：[2.1 プロバイダーの追加](./2-providers/2.1-add.md) をご確認ください
- **認証情報と供給**：上記「認証情報と供給」を参照
- **お困りの方**：[5.2 FAQ](./5-faq/5.2-questions.md) をご確認ください

## バージョン情報

- ドキュメントバージョン：v3.16.0
- 最終更新：2026-05-29
- CC Switch v3.16.0+ 対応

### v3.16.0 の注目機能

- **Codex Chat Completions プロバイダー**：Baidu Qianfan、StepFun、SiliconFlow など Chat プロトコル専用のプロバイダーを Codex 経由で利用可能 — 詳細は [2.1 プロバイダーの追加](./2-providers/2.1-add.md)
- **管理対象 CLI ツールのライフサイクル**：設定 / About で Claude / Codex / Pi のインストール、更新、一括更新、診断に対応 — 詳細は [1.5 個人設定](./1-getting-started/1.5-settings.md)
- **プロバイダーとモデルマトリクス更新**：提携プリセットを追加し、既定モデルと価格表を更新。Claude Opus は 4.8、該当する GPT 既定値は 5.5 に更新
- **軽量モード**：トレイへ最小化時にメインウィンドウを破棄、アイドル時のリソース使用量をほぼゼロに — 詳細は [1.5 個人設定](./1-getting-started/1.5-settings.md)
- **アプリ別トレイサブメニュー**：Claude / Codex / Pi の独立サブメニューで現在のプロバイダーを表示 — 詳細は [2.2 プロバイダーの切り替え](./2-providers/2.2-switch.md)
- **Skills の発見と一括更新**：SHA-256 ハッシュによる更新検出、一括更新、skills.sh 公開レジストリ検索 — 詳細は [3.3 Skills スキル管理](./3-extensions/3.3-skills.md)
- **完全URLエンドポイントモード**：高度なオプションで `base_url` を完全なアップストリームエンドポイントとして扱う — 詳細は [2.1 プロバイダーの追加](./2-providers/2.1-add.md)

## コントリビュート

Issue や PR でドキュメントの改善にご協力ください：

- [GitHub Issues](https://github.com/farion1231/cc-switch/issues)
- [GitHub Repository](https://github.com/farion1231/cc-switch)
