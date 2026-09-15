# 実装方針

## 対象

- `no_std`で利用できる同期API
- FAT12 / FAT16 / FAT32
- 論理セクタ長はBPBから取得。オンディスク位置はbyte offsetで計算し、物理blockへ分割
- 公開操作: パスによるファイル検索、任意位置読み書き、ファイル作成・伸縮・削除・rename、ディレクトリ作成・削除・rename、FATチェインの参照・確保・解放

## モジュール

- `format`: ディスク上の表現と1対1に対応する値、parser、serializer
  - `Bpb`, `RawDirEntry`, `FatEntry`, `LfnEntry`, `ShortName`
  - I/O、パス探索、キャッシュを持たない
- `volume`: BPBから導出されるFAT種別と領域配置
- `access`
  - `BlockAccess`: 物理block I/O。キャッシュを挟む最下層の境界
  - `ChainAccess`: FATエントリとクラスタチェイン。`cluster_at`を上書きするとCLMT等を利用可能
  - `DirectoryAccess`: ディレクトリエントリ走査とパス解決
  - `FileAccess`: ファイルハンドル生成と任意位置読み出し
  - `AllocationAccess`: 空きクラスタ検索、リンク、解放
  - `FatFs`: 上記機能をまとめるmarker trait
- `handle`: default実装が扱う、ファイルとディレクトリの最小メタデータ
- `write`: directory entry更新、ファイル/ディレクトリ作成・削除・rename、任意位置書き込み、伸縮

## trait案

以下のシグネチャは実装前の案。`Result`のエラーはすべて`Error<Self::Error>`に統一する。

### `BlockAccess`

物理ブロックI/Oだけを担当する。ボリューム解釈を持たない。

```rust
pub trait BlockAccess {
    type Error;

    fn block_size(&self) -> usize;
    fn read_block_at(
        &mut self,
        block: u64,
        offset: usize,
        out: &mut [u8],
    ) -> Result<(), Self::Error>;
}

pub trait BlockWrite: BlockAccess {
    fn write_block_at(
        &mut self,
        block: u64,
        offset: usize,
        data: &[u8],
    ) -> Result<(), Self::Error>;
}
```

`read_block_at`を部分読み出し可能にする理由:

- default実装内で最大4096 byteのsector bufferをスタック確保しない
- crate側が作業バッファを保持しないため、再入可能性と所有関係が明確
- decoratorまたは実装内部でblock cacheを保持できる

`offset + out.len() <= block_size()`は実装側の契約とする。読み取り専用デバイスにダミーのwriteを要求しないため、書き込みは`BlockWrite`へ分離する。

全block I/Oしか提供しないデバイスを直接包む場合、adapter側に1 blockの作業領域が必要になる。`read_block_at`を採用するとこの領域は上位traitから消せる一方、下位adapterまたはcacheが所有することになる。このトレードオフは実装前に確認する。

採用方針:

- default実装は、同じblock内または連続領域を扱える場面で要求を可能な限りまとめ、返されたsliceを上位で切り分ける
- 全block I/Oしかないadapterは、残る部分アクセスのための作業領域を所有する
- 独立したAPI呼び出し間の小さなアクセスをまとめる責任は、`BlockAccess`実装内またはdecoratorのcacheが持つ
- cacheなしでも正しく動作することは保証するが、独立アクセス間の再読み出し回避は保証しない

### `ChainAccess: BlockAccess`

FATとクラスタチェインを担当する。

```rust
pub trait ChainAccess: BlockAccess {
    // 必須。BPBから導出済みの不変な配置情報を返す。
    fn volume(&self) -> &Volume;

    // 以下はdefault impl。
    fn read_volume_at(&mut self, byte_offset: u64, out: &mut [u8]) -> Result<()>;
    fn read_fat_entry(&mut self, cluster: Cluster) -> Result<FatEntry>;
    fn next_cluster(&mut self, cluster: Cluster) -> Result<Option<Cluster>>;

    // 汎用チェイン探索。FS全体のチェイン索引を持つ場合の上書き箇所。
    fn cluster_at(&mut self, first: Cluster, index: u32)
        -> Result<Option<Cluster>>;

    fn read_chain_at(
        &mut self,
        first: Cluster,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize>;
}
```

FAT12の12 bit entryがblock境界を跨ぐ場合も、`read_volume_at`が複数blockへ分割する。FAT32では上位4 bitを値として扱わない。

### `DirectoryAccess: ChainAccess`

ディレクトリ走査とパス解決を担当する。

```rust
pub trait DirectoryAccess: ChainAccess {
    type DirectoryHandle: DirectoryHandle + From<DirectoryInfo>;

    // 以下はdefault impl。
    fn root_directory(&self) -> Self::DirectoryHandle;
    fn read_directory_entry(
        &mut self,
        directory: &Self::DirectoryHandle,
        index: u32,
    ) -> Result<Option<RawDirEntry>>;
    fn find_entry(
        &mut self,
        directory: &Self::DirectoryHandle,
        name: &str,
    ) -> Result<FoundEntry>;
    fn open_directory(&mut self, path: &str) -> Result<Self::DirectoryHandle>;
}
```

`find_entry`がLFN列のchecksum/orderを検証し、壊れたLFN列だけを捨てて対応するshort nameも照合する。FAT12/16の固定root領域とFAT32のroot clusterの違いは`DirectoryLocation`へ閉じ込める。

### `FileAccess: DirectoryAccess`

ファイルハンドル生成と読み出しを担当する。

```rust
pub trait FileAccess: DirectoryAccess {
    type FileHandle: FileHandle + From<FileInfo>;

    // default impl。
    fn open_file(&mut self, path: &str) -> Result<Self::FileHandle>;

    // defaultはChainAccess::cluster_atを呼ぶ。
    // CLMTをFileHandleに持たせる実装はここだけを上書きする。
    fn resolve_file_cluster(
        &mut self,
        file: &mut Self::FileHandle,
        index: u32,
    ) -> Result<Option<Cluster>>;

    // defaultはChainAccess::next_clusterを呼ぶ。
    // CLMTで連続read中のFAT参照も省く場合は上書きする。
    fn resolve_next_file_cluster(
        &mut self,
        file: &mut Self::FileHandle,
        current: Cluster,
        next_index: u32,
    ) -> Result<Option<Cluster>>;

    // resolve_file_clusterを経由し、EOFで読み出し長を切り詰める。
    fn read_file_at(
        &mut self,
        file: &mut Self::FileHandle,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize>;
}
```

CLMTはファイルごとの情報なので、`read_file_at`から直接`read_chain_at`を呼ばない。`resolve_file_cluster`でクラスタを解決してから該当クラスタ内を読む。これにより独自`FileHandle`へ固定長テーブル、外部arenaのキー、動的領域などを追加できる。

`FileHandle`が要求するviewは次だけにする。

```rust
pub trait FileHandle {
    fn file_info(&self) -> &FileInfo;
}
```

default実装はハンドルの内部表現を変更しない。`From<FileInfo>`でopen時の生成だけを可能にする。

関連型を使う理由は、同じFS実装に複数種類のハンドル実装が生えてmethod解決が曖昧になることを避けるため。trait型引数へdefault handle型を指定すれば`impl FileAccess for MyFs {}`まで短縮できるが、複数の`FileAccess<H>`実装が可能になり、上位traitの型関係も複雑になるため採用しない。

### `AllocationAccess: ChainAccess + BlockWrite`

FATチェインの変更だけを担当する。ディレクトリエントリ作成やファイル長更新は担当しない。

```rust
pub trait AllocationAccess: ChainAccess + BlockWrite {
    // 以下はdefault impl。
    fn write_fat_entry(&mut self, cluster: Cluster, value: FatEntry) -> Result<()>;
    fn find_free_cluster(&mut self, start: Option<Cluster>) -> Result<Cluster>;
    fn allocate_cluster(&mut self, after: Option<Cluster>) -> Result<Cluster>;
    fn free_chain(&mut self, first: Cluster) -> Result<()>;
}
```

`write_fat_entry`はBPBのmirroring設定に従う。FAT32 entryの上位4 bitはread-modify-writeで保存する。`allocate_cluster`は新clusterをEOCにしてから既存chainへ接続し、中断時に既存chainを壊しにくい順に書く。

`allocate_cluster`は新clusterをzero clearしてから既存chainへ接続し、再利用データがファイルのgapや新規directoryから露出しない状態を保つ。

### `FatFs`

```rust
pub trait FatFs: FileWrite
where
    Self::FileHandle: MutableFileHandle,
{}

impl<T> FatFs for T
where
    T: FileWrite,
    T::FileHandle: MutableFileHandle,
{}
```

機能を束ねるだけのmarker trait。固有のdefault methodは置かない。読み取り専用実装は`FileAccess`まで実装でき、`BlockWrite`の偽実装を必要としない。

### `DirectoryWrite` / `FileWrite`

- `DirectoryWrite`: 空きentry列の検索、LFN/short alias生成、directory chainの伸長、ファイル/ディレクトリ作成・削除・rename
- `FileWrite`: `write_file_at`, `truncate_file`、directory entry上の先頭cluster/file size更新
- `MutableFileHandle`: 書き込み後の`FileInfo`更新だけを要求。読み取り専用`FileHandle`とは分離
- FAT32 allocation後: FSInfoのfree count/next freeを仕様上のunknown値へ更新
- directory rename: 新entryを作成してから旧entryを削除。親が変わる場合は`..`を更新し、子孫への移動は拒否

## 利用側に必要なimpl

crate提供の基本ハンドルを使う最小構成:

```rust
struct MyFileSystem<D> {
    device: D,
    volume: Volume,
}

impl<D: BlockAccess> BlockAccess for MyFileSystem<D> {
    type Error = D::Error;
    // block_size/read_block_atをdeviceへ委譲
}

impl<D: BlockAccess> ChainAccess for MyFileSystem<D> {
    fn volume(&self) -> &Volume { &self.volume }
}

impl<D: BlockAccess> DirectoryAccess for MyFileSystem<D> {
    type DirectoryHandle = BasicDirectoryHandle;
}

impl<D: BlockAccess> FileAccess for MyFileSystem<D> {
    type FileHandle = BasicFileHandle;
}
```

書き込みも行う場合だけ次を追加する。

```rust
impl<D: BlockWrite> BlockWrite for MyFileSystem<D> {
    // write_block_atをdeviceへ委譲
}

impl<D: BlockWrite> AllocationAccess for MyFileSystem<D> {}
impl<D: BlockWrite> DirectoryWrite for MyFileSystem<D> {}
impl<D: BlockWrite> FileWrite for MyFileSystem<D> {}
```

つまり、必須の実処理はblock I/Oの委譲と`volume()`だけ。ハンドル関連は型指定だけで、FAT、directory、file、allocationの通常処理はdefault implになる。

crateには同じ構成の`FileSystem<D>`も用意し、`FileSystem::new(device)`でboot sectorを読み、`Bpb`と`Volume`を構築する。失敗しうるので実際の返り値は`Result<Self, Error<D::Error>>`とする。

`FileSystem::new`は渡されたdeviceのbyte 0をvolume先頭とする。MBR/GPTのpartition探索は別責任とし、partition範囲を切り出す`BlockAccess` decoratorで対応する。

## default implの呼び出し経路

```text
open_file(path)
  -> root_directory
  -> find_entry / open_directory
     -> read_directory_entry
        -> read_chain_at (通常directory) または read_volume_at (固定root)
           -> cluster_at -> read_fat_entry -> read_block_at

read_file_at(handle, offset, out)
  -> resolve_file_cluster(handle, cluster_index)  // CLMT差し替え点
     -> cluster_at -> read_fat_entry              // 既定動作
  -> resolve_next_file_cluster                    // 連続readのCLMT差し替え点
     -> next_cluster -> read_fat_entry             // 既定動作
  -> read_volume_at -> read_block_at              // cache差し替え点
```

## 拡張境界

- ブロックキャッシュ: `BlockAccess::read_block_at` / `BlockWrite::write_block_at`の実装内、または同traitを実装するdecorator
- CLMT: ファイル単位なら`FileAccess::{resolve_file_cluster, resolve_next_file_cluster}`、FS共通なら`ChainAccess::cluster_at`を上書き
- 独自ハンドル: `FileAccess::FileHandle` / `DirectoryAccess::DirectoryHandle`の関連型。ハンドルは返り値にのみ現れ、各traitで要求する小さなview traitを実装する
- `file.read_at()`形式: FSへの`&mut`参照を保持する別wrapperとして追加可能。基本ハンドルはFSを借用せず、複数ハンドルを同時保持できる形を維持

## エラー

- 下層I/Oエラーは`Error::Io`で保持
- 壊れたオンディスク値、未対応形式、検索失敗、容量不足を区別
- parserは範囲外アクセスや予約値を受理しない

想定variant:

```rust
pub enum Error<E> {
    Io(E),
    InvalidFormat(FormatError),
    CorruptChain,
    NotFound,
    NotAFile,
    NotADirectory,
    InvalidPath,
    NoSpace,
    AlreadyExists,
    DirectoryFull,
    NameTooLong,
    InvalidName,
    NotEmpty,
    ReadOnly,
    OutOfBounds,
}
```

## 実装前に固定する挙動

- `open_file`は絶対・相対の両方をroot起点として扱う。空要素と`.`は無視し、`..`はオンディスクentryとして解決する
- short nameはASCII case-insensitive、LFNはUnicode正規化を行わずUTF-16として一致比較する
- `read_file_at`はEOF以降で`Ok(0)`、EOFを跨ぐ場合は短い読み出し
- directory走査はvolume label、削除済みentryを返さない
- bad/reserved/free clusterがchain途中に出た場合は`CorruptChain`
- FAT32のactive FAT/mirroringを扱うため、`Bpb`にはextended flagsも保持する

## 後から無理が出やすい点

### FSを借用するファイルオブジェクト

`OpenedFile<'a, F> { fs: &'a mut F, handle: F::FileHandle }`を追加すれば`file.read_at()`は実現できる。ただし、その間FS全体が排他的に借用され、同時に別ファイルを開けない。基本APIにはせず、短命な便宜wrapperとして後から追加する。

### CLMTの構築タイミング

defaultの`open_file`は`From<FileInfo>`しか要求しないため、CLMT構築にI/Oは行わない。独自実装は次のどちらかを選べる。

- `resolve_file_cluster`の初回呼び出しでlazy構築
- `open_file`を上書きし、default相当の検索後にeager構築

default処理を部分的に再利用するため、パス検索本体は`find_entry_by_path`のような下位default methodとして分離する予定。

Rustでは上書きしたtrait methodから、そのtraitのdefault実装を`super`のように呼べない。拡張側で既定処理の一部を複製しなくて済むよう、公開methodを次の二層にする。

- `find_entry_by_path`: パス検索だけを行う再利用単位
- `open_file`: 検索結果を`FileHandle`へ変換する便宜操作
- `cluster_at`: 通常のFATチェイン探索
- `resolve_file_cluster`: ハンドル固有の索引を適用する差し替え点。fallbackは明示的に`cluster_at`を呼べる

必要ならtrait methodのdefault本体を同名の`access::default::*`関数へ置き、上書き実装から明示的に呼べるようにする。実装時に重複が実際に生じる箇所だけ用意し、全methodを機械的に二重化しない。

### handle traitの可変情報

default実装がファイル位置を保持しない`read_at`方式では`FileHandle`のimmutableなviewだけを使う。書き込みは別の`MutableFileHandle`で先頭cluster、size、attributes、directory entry位置の更新を要求する。将来sequential cursorを追加する場合は、これらへcursor位置を混ぜず別traitまたはwrapperを設ける。

### allocationと一貫性

FATにはtransactionがないため、複数FAT copy、クラスタリンク、directory entry、file sizeを跨ぐ完全な原子性は提供しない。既存chainを壊しにくい順序を採用し、伸長は新clusterの初期化後に接続、縮小はfile size更新後に余剰chainを解放、削除はentryを不可視化してからchainを解放、renameは新entryを公開してから旧entryを削除する。

## テスト

- formatの既知byte列とのround-trip
- FAT12 / FAT16 / FAT32のFATエントリ境界
- メモリ上の最小FATイメージによるLFN/短名のパス検索とクラスタ境界を跨ぐread
- allocationのリンク・解放
- ファイル作成、gapを含む伸長、縮小、read-only拒否
- directory chain伸長、ファイル/ディレクトリ削除、親を跨ぐrename、循環拒否

時刻更新とFSInfoの空き数最適化は含めない。FSInfoはallocation後にunknownへ戻し、他実装が古い値を利用しない状態を保つ。
