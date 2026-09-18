#![allow(clippy::fn_params_excessive_bools)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::needless_lifetimes)]
#![allow(clippy::redundant_closure)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::trim_split_whitespace)]
//! Named native adapters for hook bodies the typed compiler cannot emit.
//!
//! Bound by `bodySha256` + field. The catalog dump is compiled into the
//! library so `dump-adapters` can publish `adapters.json`. T3.1's Native
//! backend evaluates these adapters when a hook id is not in typed IR.

use serde::Serialize;

const ADAPTERS_KIND: &str = "native-hook-adapters";
const ADAPTERS_VERSION: u64 = 1;

struct AdapterMeta {
    body_sha256: &'static str,
    field: &'static str,
    representative_hook_id: &'static str,
    reason: &'static str,
}

macro_rules! meta {
    ($sha:literal, $field:literal, $id:literal, $reason:literal) => {
        AdapterMeta {
            body_sha256: $sha,
            field: $field,
            representative_hook_id: $id,
            reason: $reason,
        }
    };
}

const ADAPTERS: &[AdapterMeta] = &[
    meta!(
        "16eb363f9957097622cbe2ed626cfc4859c24b797e5e2acff58cf40f4f9fbbad",
        "filterTemplateSuggestions",
        "direnv#filterTemplateSuggestions#0",
        "factory helper e disagrees across call sites"
    ),
    meta!(
        "06cf60a4db4009e789aa2efbfd2e826a8fc7be284a20bc56a257ff0ba2524a33",
        "postProcess",
        "bunx#postProcess#0",
        "free variable n is the closed-over command list"
    ),
    meta!(
        "08c4a4a0e91a87111822336c4aba36b82aa89bef9faac02831e7c1bafb0eeb1c",
        "postProcess",
        "just#postProcess#1",
        "helper c references console"
    ),
    meta!(
        "1db94727eae43e429172bdb647fcdce51562bc830797579fdd211681742b949b",
        "postProcess",
        "rush#postProcess#10",
        "helper P uses a non-simple parameter shape"
    ),
    meta!(
        "20afa25251f32cd950d69142214fb6b725cca32e75676c486610ddabb6200d58",
        "postProcess",
        "dcli#postProcess#0",
        "Array.sort plus Date formatting are outside typed IR"
    ),
    meta!(
        "40446df6c189303738556d8b980c04a816d639d9ed1576c2f1eee6df70b1ff3d",
        "postProcess",
        "limactl#postProcess#0",
        "factory helper e disagrees across call sites"
    ),
    meta!(
        "40f323a2aaed65f89013fd38fc15cd73181b00b577418036d25981f47b98ebb8",
        "postProcess",
        "git#postProcess#16",
        "Array.reduce is not in the typed allowlist"
    ),
    meta!(
        "46cfb8ef98d11e187a3fb9a8756cf911956bd5f5bc48d22ce938faee7860f35b",
        "postProcess",
        "react-native#postProcess#5",
        "Array.reduce is not in the typed allowlist"
    ),
    meta!(
        "47dd9ebffc5ba15a140bafd0c09167f71b38de57bb8782036e4edd0ba8049e89",
        "postProcess",
        "asdf#postProcess#1",
        "factory helper n disagrees across call sites"
    ),
    meta!(
        "566065d403b4e22dba74ec84e8e64ef2c541ce984fef4051e41baaf5a8941120",
        "postProcess",
        "cargo#postProcess#261",
        "helper le references console"
    ),
    meta!(
        "57813e77ada7aad9de83a53034b622f02d180c989cc9b6688aec5f271b614906",
        "postProcess",
        "fnm#postProcess#0",
        "free variables g/u plus Map are outside typed IR"
    ),
    meta!(
        "581af18c6c4ccb098615187809a15e6892771178da47cb9c7016272dc18fb572",
        "postProcess",
        "dcli#postProcess#1",
        "Array.sort plus Date formatting are outside typed IR"
    ),
    meta!(
        "599dd22a2fdc7873a899448dd4c11ffce9c0742c9a64e930819f57990b0c126f",
        "postProcess",
        "rustup#postProcess#8",
        "Date is not representable in typed IR"
    ),
    meta!(
        "5b988a428bdc5060c8f035d9d480ea81be6e411de28351871118d9e8bc7d4978",
        "postProcess",
        "tldr#postProcess#0",
        "dynamic RegExp is outside typed IR"
    ),
    meta!(
        "5cde47b72c7c8040bea8146f4c5bd7267bb7f07bb53b84964901d097f94b2d37",
        "postProcess",
        "rustup#postProcess#1",
        "factory helper o disagrees across call sites"
    ),
    meta!(
        "61d0b086797b88cb187f1bcc5b3a82c66b081a9756fde0e10cfde9fb90f5eabf",
        "postProcess",
        "rustup#postProcess#15",
        "factory helper o disagrees across call sites"
    ),
    meta!(
        "66349787d8ae1c1f6b8b895a43758316cff3ef10d8f8c70a9eaace0b079c937c",
        "postProcess",
        "pre-commit#postProcess#11",
        "YAML helper _s is not inlinable"
    ),
    meta!(
        "7684a7c68524c7c00f9309417061576eb590152382aea725b4687ea6824cfe6c",
        "postProcess",
        "brew#postProcess#15",
        "factory helper i disagrees across call sites"
    ),
    meta!(
        "775c0c96e9204671a2a03e3ec4839bfd73f1e9fdec79ce11f7d2b1abf9120c67",
        "postProcess",
        "deno#postProcess#0",
        "array of matcher arrows is not representable"
    ),
    meta!(
        "83c37762b8e6a3fb3dfef347ac4d34f35174c2ae0483908a2101a1c521a2ef1f",
        "postProcess",
        "yarn#postProcess#11",
        "Object.assign is not representable"
    ),
    meta!(
        "86847ea037183bc60ece6f12b5dc2764beb807a76a4930c2a28412a63d96e174",
        "postProcess",
        "yo#postProcess#0",
        "nested arrows in the title-case helper"
    ),
    meta!(
        "86e2f49405e79a81d3c4fd402009b27d5e352d1e80d69ee496dda22abc8c65b2",
        "postProcess",
        "task/taskwarrior#postProcess#0",
        "module helpers use Array.reduce"
    ),
    meta!(
        "97aa92d325bb5a2273a65c8204a97bbe4be8598b9d41cf129d8fa554d510fbf3",
        "postProcess",
        "asdf#postProcess#11",
        "factory helper n disagrees across call sites"
    ),
    meta!(
        "9e08e5a9bac41e7def677da3f7fc812fc90377d437d8121a05c1f661fecfb32a",
        "postProcess",
        "open#postProcess#1",
        "new Map is outside the typed Set-only constructor"
    ),
    meta!(
        "a148afe726cd7409a53bcfe722708bc9e35a5e9f7a409817a22783fe77527df3",
        "postProcess",
        "snaplet#postProcess#0",
        "typed JSON objects do not expose .src"
    ),
    meta!(
        "a2343033ad7ec234dc9cd89f566c5c9024a725655738aa583562d0c4557f0de5",
        "postProcess",
        "sake#postProcess#1",
        "Array.reduce is not in the typed allowlist"
    ),
    meta!(
        "a8bce6f4bcf79ed952f5325ec0b5c7f1041eb0e10320fbd9beb629efb4071bbe",
        "postProcess",
        "task/taskwarrior#postProcess#33",
        "module helpers use Array.reduce"
    ),
    meta!(
        "b0575d96c049ffec2de9ce491f5319e3ad61c3c908d0dec5554aeda00ed971c7",
        "postProcess",
        "tailscale#postProcess#0",
        "factory helper s disagrees across call sites"
    ),
    meta!(
        "b3739a280026f1e961efaf2ded7bf918e22c909af433ec86f2eec02c6aadeca1",
        "postProcess",
        "snaplet#postProcess#2",
        "typed JSON objects do not expose .name on helper rows"
    ),
    meta!(
        "b3b72135b863b89e29cf44274d5e381daf51d9c53c3313cc68096fc1f2af8177",
        "postProcess",
        "pnpm#postProcess#10",
        "Object.assign is not representable"
    ),
    meta!(
        "baa2ab8ac2cd27aaf62bd6b24ddd1734a395dcbab0dfed6ad6ddf108d19ac709",
        "postProcess",
        "tccutil#postProcess#0",
        "new Map is outside the typed Set-only constructor"
    ),
    meta!(
        "bcb6a45f08256f9fd34e48ecbfb13cd9b5d825e96e308131538d16cb2f5400bc",
        "postProcess",
        "turbo#postProcess#1",
        "the in operator is unsupported"
    ),
    meta!(
        "c3fac3c2f48178fe13af582ab93d7b9bfc579951c78a805946b6563c415e8848",
        "postProcess",
        "just#postProcess#3",
        "helper c references console"
    ),
    meta!(
        "ca1383d43cf172af5d0f743e6933ef8e7154147c5ff549ad1b6fc4a6e88e90be",
        "postProcess",
        "task/taskwarrior#postProcess#51",
        "module helpers use Array.reduce"
    ),
    meta!(
        "d060a61ead077da1a1b2a779f8670f87f55010a22a34bdd3f8b8545378d0833e",
        "postProcess",
        "pre-commit#postProcess#10",
        "Array.reduce is not in the typed allowlist"
    ),
    meta!(
        "d3e93ba8a10aa6f82f74dd3df6cc51e81d815eb8e36072ed084d9a2dddcd6f61",
        "postProcess",
        "just#postProcess#5",
        "helper c references console"
    ),
    meta!(
        "de5329d88e6a3667531eace7c10a57034bb8ec9e643f910095a0baa80235de20",
        "postProcess",
        "git#postProcess#8",
        "inlined helper AST exceeds the 2048-node cap"
    ),
    meta!(
        "deba5217b1bd370de0396ae66042361a21ebd372bc0e308858dedca94ec79986",
        "postProcess",
        "vr#postProcess#0",
        "conditional object spread is not a suggestion object"
    ),
    meta!(
        "e0ff02cf4190764a1e6043c0ace62aaf37a63bffd693744f1072c87df04c710d",
        "postProcess",
        "kubecolor#postProcess#40",
        "free variable a is a module helper"
    ),
    meta!(
        "e66f8b0736bf23a063bda8970e1de8b9ac8f72328a53ac15671982af49b7f468",
        "postProcess",
        "snaplet#postProcess#4",
        "typed JSON objects do not expose .status"
    ),
    meta!(
        "e8289375fc0901b1f363b7525760614e99748125598c35e172710d7446292842",
        "postProcess",
        "deno#postProcess#3",
        "helper ie is recursive"
    ),
    meta!(
        "e8a02695049c0e386fb7345f0e54d5bd2d2ce1c04481e228e5e9f2bf4f583c14",
        "postProcess",
        "gource#postProcess#0",
        "spread on a partial suggestion is a type mismatch"
    ),
    meta!(
        "f04211ce9cc3b53755c0429dfc0fcaac7b07fe7d4a7d9021fc2c3c11cf5f6adb",
        "postProcess",
        "cf#postProcess#0",
        "factory helper s disagrees across call sites"
    ),
    meta!(
        "03b126c52218b618b258b4113e23cf47ef5a0197646c1f48f7983f6bf146ead8",
        "custom",
        "make#custom#0",
        "only new Set is representable"
    ),
    meta!(
        "082bc08b4bdda4806561b75e8cc0731eebc9d4400f2113e5c7be189485a43558",
        "custom",
        "scc#custom#0",
        "factory argument t has disagreeing call-site values"
    ),
    meta!(
        "111e4cf667dc7a906760475f6dd4eca45bb903941a433cc4775e2f8c2cdb7d40",
        "custom",
        "man#custom#0",
        "factory argument t has disagreeing call-site values"
    ),
    meta!(
        "1479ee375b43e8542737587098c3443c5b480cddafb76ed3a5e1d458c119e608",
        "custom",
        "pkgutil#custom#0",
        "only allowlisted methods may be called"
    ),
    meta!(
        "15969249b10aedd33ec7f60746954920fc08b8e25e0671ca53aaf51ac946ce37",
        "custom",
        "nx#custom#15",
        "executeCommand is only valid as a callee"
    ),
    meta!(
        "1a93472e0d5a250a7a3ba467851694ca669a32bb9d509dfabcd5528b74702941",
        "custom",
        "git#custom#59",
        "AST node ArrowFunctionExpression is not representable"
    ),
    meta!(
        "1b1c58278249c1860643584345a4e2999f6f809c8700b284e7a26ce6cbd6fa20",
        "custom",
        "spring#custom#0",
        "only allowlisted methods may be called"
    ),
    meta!(
        "1ddb3946a1205a82600a80ab80897691a797873651f7afb8ff97a0159c139fe7",
        "custom",
        "chezmoi#custom#0",
        "hook parameters must be simple identifiers or destructuring patterns"
    ),
    meta!(
        "20e727a6b059f8d895ae13dcba72e4308f8935db13897c58057e7805f0182e1e",
        "custom",
        "deno#custom#19",
        "executeCommand is only valid as a callee"
    ),
    meta!(
        "2176333a01195561f613dca132852a71d43c790bdd316c99b3533f02ba9fff46",
        "custom",
        "esbuild#custom#4",
        "hook parameters must be simple identifiers or destructuring patterns"
    ),
    meta!(
        "25d872a1238176acad55fdc6ab4925964829c5b33510679a754b1de0a5af8008",
        "custom",
        "scp#custom#1",
        "helper d is recursive"
    ),
    meta!(
        "2a1148465e4a6f039eaf6a53448cf72d9ddcbe1de292c76c631364edda8ef870",
        "custom",
        "ykman#custom#0",
        "only new Set is representable"
    ),
    meta!(
        "2d5b190a7bae97d96654250225aaefb33dde63770d47876954c52e32ddbc255e",
        "custom",
        "firefox#custom#0",
        "identifier Number is only valid as a callee"
    ),
    meta!(
        "2ed14222e43605a4615b0d098db0dca8fb7016d90702476658c98a8d3397be57",
        "custom",
        "npm#custom#1",
        "spread calls are unsupported"
    ),
    meta!(
        "2fadd1a502cd5f4ba4a55f8bdc1ee236ea4d572c77e2188e7cdcb3c77856036d",
        "custom",
        "file#custom#0",
        "hook parameters must be simple identifiers or destructuring patterns"
    ),
    meta!(
        "353cc07bf9fb8f63179771f62005442bc7b3c5f0e22102ff5f567156226a0366",
        "custom",
        "pnpm#custom#1",
        "spread calls are unsupported"
    ),
    meta!(
        "4df08d834f2264ea2b300b6b8e847fe753da5c9a5ecfaebf87596a84d584625d",
        "custom",
        "spring#custom#5",
        "only allowlisted methods may be called"
    ),
    meta!(
        "4e14f9a18a339f1a0ff2799de460a98abbbd4f754e729069e600065d6b23bf9f",
        "custom",
        "dscacheutil#custom#1",
        "only allowlisted methods may be called"
    ),
    meta!(
        "5104550a1de4f3c8210aefeca860ac9ba05a66d588d3940c98b15fb473090866",
        "custom",
        "tldr#custom#3",
        "free identifier n is not representable"
    ),
    meta!(
        "5395d7fb1d0b608ce456527e703936e65ead88c78e1d64d17e5223015e947473",
        "custom",
        "rush#custom#0",
        "spread calls are unsupported"
    ),
    meta!(
        "5693d96360a2e105298439a7a22abbde058eaa41e74ef120f1b1d722e2eedbf6",
        "custom",
        "fig/1.0.0#custom#3",
        "factory argument i has disagreeing call-site values"
    ),
    meta!(
        "5aa8d63e46c44f4cf86dbb62c9035c2a33506a4aa04da4b928adf5ef102b2a9a",
        "custom",
        "codesign#custom#0",
        "hook parameters must be simple identifiers or destructuring patterns"
    ),
    meta!(
        "5c415cacc2e9cfe61ff86d5ec428d6b0089216ab3e13fd1acb53ed513b9e2606",
        "custom",
        "chezmoi#custom#24",
        ".map is supported only on arrays"
    ),
    meta!(
        "61c3b8a8846c19ae82d4c833b36fec1e52cebac1e8ebbd976587556ea427847d",
        "custom",
        "mosh#custom#1",
        "helper c is recursive"
    ),
    meta!(
        "63f4199d5d1d44c71d013364bf8dbb477411f11d776cde35821ca712569034c0",
        "custom",
        "scc#custom#8",
        "factory argument n has disagreeing call-site values"
    ),
    meta!(
        "63f7ebc4331aeef1adf022c2217ba0c013735b99a32015c475374c61c076ff95",
        "custom",
        "oxlint#custom#3",
        "method .charCodeAt is not in the typed hook allowlist"
    ),
    meta!(
        "661801a6a065f6a47a99557d48b30b8ddaf73a2b08a407d4db2b1434196c8fc9",
        "custom",
        "gh#custom#26",
        "factory argument n has disagreeing call-site values"
    ),
    meta!(
        "7118b51f5ab4abf816ec0fd32b660d4495361defafb617d48a91bab93e74d914",
        "custom",
        "swift#custom#0",
        "hook parameters must be simple identifiers or destructuring patterns"
    ),
    meta!(
        "7238b45ae90afed81b58fe2198530b0074949e85863636d179d805d7fe23d1c0",
        "custom",
        "airflow#custom#0",
        "hook parameters must be simple identifiers or destructuring patterns"
    ),
    meta!(
        "7cdfa0e8c803cb9e4c6176f51aa7730cdd864347528547cf8291722c94fd8784",
        "custom",
        "nx#custom#11",
        "factory argument n has disagreeing call-site values"
    ),
    meta!(
        "7d44d284dc19be5e46e12cea613a04edbc98e806a318d94d52a38b0c296c9927",
        "custom",
        "oxlint#custom#2",
        "free identifier g is not representable"
    ),
    meta!(
        "83fee23ebcadbf1d63827dd335c7cf0b8a40bbc3d7266022e9ad49e4e231e1ae",
        "custom",
        "esbuild#custom#0",
        "factory argument r has disagreeing call-site values"
    ),
    meta!(
        "84025fe23bbe188d73039e3d402687d1f0b39418ad70f09a1e4e6ed495038bb3",
        "custom",
        "deno#custom#10",
        "factory argument t has disagreeing call-site values"
    ),
    meta!(
        "9523c6ca2b912383473caefc1b9f7eea0a712e3e648b161621a7b945277df483",
        "custom",
        "goto#custom#0",
        "only new Set is representable"
    ),
    meta!(
        "9c22acfa8a0ec3748d93b0fa87f7aca3df2f20e71e41da53f60dacae3209e570",
        "custom",
        "git-flow#custom#0",
        "executeCommand is only valid as a callee"
    ),
    meta!(
        "a0947a29c5844aa06f36b4666c32b578a97663219aa1b47687c2916c0102d57b",
        "custom",
        "rsync#custom#1",
        "helper c is recursive"
    ),
    meta!(
        "a22d06a56581e86c16f9dc836857cc1c418d4c19cf441b9207b8b333aac56608",
        "custom",
        "spring#custom#1",
        "only allowlisted methods may be called"
    ),
    meta!(
        "a66254e7246fa779b200beb5f1907554b3cdafd35415f8bb20cfe87ae545a01c",
        "custom",
        "git#custom#3",
        "factory argument t has disagreeing call-site values"
    ),
    meta!(
        "ad600744147080b3bbb0ee8363c354bcc1ec7198ac5a9b16ccbf6285f70b82eb",
        "custom",
        "@magnolia/cli#custom#0",
        "factory argument n has disagreeing call-site values"
    ),
    meta!(
        "add93fb6b6f290a3c95e19f260fff11c67a5039441066c0c3e97c39663b7d9fb",
        "custom",
        "twilio#custom#0",
        "factory argument i has disagreeing call-site values"
    ),
    meta!(
        "aeef663f3f913be3aae37442b98f532b8495d5cfbe89c42e731c48d60e2fa291",
        "custom",
        "nx#custom#7",
        "executeCommand is only valid as a callee"
    ),
    meta!(
        "b5a3021322137127b1384a405317a1e561d6f8da31e33d391262f8bb12350849",
        "custom",
        "rich#custom#0",
        "helper y has no statically visible initializer"
    ),
    meta!(
        "b78df2cf1a8bb3a6e158fed0c9f86cc6ab1a59af3b1c78902a272db13ffe65e1",
        "custom",
        "man#custom#4",
        "free identifier Date is not representable"
    ),
    meta!(
        "c7306386562b66c67f69ba7a123f8a4116b4e523cbe4160c6e20193fda412fab",
        "custom",
        "mosh#custom#0",
        ".indexOf is supported only on strings and arrays"
    ),
    meta!(
        "cd92b7bcc8f44295973c4b7bfc5a4851b633bff0e57f4ee98cfe91680c2b82ac",
        "custom",
        "ni#custom#0",
        "spread calls are unsupported"
    ),
    meta!(
        "d928c7d8792f6d2a77531253fa3a5cce3b7cf3b0f255bddb090cbd33dd38cb12",
        "custom",
        "ssh#custom#0",
        ".indexOf is supported only on strings and arrays"
    ),
    meta!(
        "d972eb5f899bdf11a9a76db260c3984cd04d2ab1d4d0a2d98661c1f1a56530b3",
        "custom",
        "osqueryi#custom#0",
        "hook parameters must be simple identifiers or destructuring patterns"
    ),
    meta!(
        "dc40f21f14541d8f6743f532137900894daa56f7d8fede9edadae126dfc8074b",
        "custom",
        "bun#custom#15",
        "factory argument t has disagreeing call-site values"
    ),
    meta!(
        "ddf08d87219a1404bfeec7328e0a92e5d615202ab064e51081c6a2181619777e",
        "custom",
        "cargo#custom#102",
        "free identifier Intl is not representable"
    ),
    meta!(
        "e0f755fcbcdd4aa9db41d69f013e92b961ad64f347b05a750718d30d8a851634",
        "custom",
        "cargo#custom#1",
        "factory argument e has disagreeing call-site values"
    ),
    meta!(
        "e299db05c81077e0ef9b1344daef841fa83f1849ad704e508af664ed388ce510",
        "custom",
        "bun#custom#11",
        "spread calls are unsupported"
    ),
    meta!(
        "ece47a53affb3d4f9b613f64e9e2e6b2dfa848b291ac79d4618a66c06f4769d6",
        "custom",
        "rsync#custom#0",
        ".indexOf is supported only on strings and arrays"
    ),
    meta!(
        "f4cb5caf468f2f8af4b0b33e6bb43d9ccf8af8bc4d230519b947b17cd7043111",
        "custom",
        "rich#custom#2",
        "statement SwitchStatement is not representable"
    ),
    meta!(
        "f6c6c160699c68fe7cf0500219e46b8c6c7b327c7db559d3632e1de31cf40fc4",
        "custom",
        "yarn#custom#1",
        "spread calls are unsupported"
    ),
    meta!(
        "faf81d363c15195974dc41b68bfaeb799bc2a67b0d6143555695df0e4bce9827",
        "custom",
        "fig/2.0.0#custom#14",
        "factory argument t has disagreeing call-site values"
    ),
    meta!(
        "fb26b35043ba67c50e86361dc595f1ca096a2c9b46796349d8eafbee0939e4c7",
        "custom",
        "dscl#custom#0",
        "free identifier d is not representable"
    ),
    meta!(
        "fde524eda218ac28c5bce41a27df6ac6bdc9254da9e25f8dcb5880c8b7026c18",
        "custom",
        "nx#custom#20",
        "spread calls are unsupported"
    ),
    meta!(
        "fde95bc4219fd9ff0a036e870a458c96dd54ba80ebe06fb725b1224fd0b51ebf",
        "custom",
        "cargo#custom#104",
        "factory argument t has disagreeing call-site values"
    ),
    meta!(
        "04bdcd95f16944d756d2ca38dfb448f803ff71d43d430bbf8eb8cfbc8c4652d1",
        "generateSpec",
        "yarn#generateSpec#16",
        "hook parameters must be simple identifiers"
    ),
    meta!(
        "124fc96c760bc04b02dc9baec9d6fd550c4e3823948891bee9c1bf667b9eb902",
        "generateSpec",
        "kamal#generateSpec#0",
        "free identifier r is not representable"
    ),
    meta!(
        "18d24ae447e700e8b7e6865d53ebf24373d36e131954e26bc0a36c2819d50d79",
        "generateSpec",
        "sake#generateSpec#0",
        "method .reduce is not in the typed hook allowlist"
    ),
    meta!(
        "1d6fbd6ace2331fa39dbab0f60383af2495789fef4e4a2fabdabdb463aa9df95",
        "generateSpec",
        "drush#generateSpec#0",
        "identifier Array is only valid as a callee"
    ),
    meta!(
        "3acab33d9a4fa0871aa2a9d41fbed3b5e02743b609bb4381524cb4ab5c9ba9f5",
        "generateSpec",
        "rails#generateSpec#0",
        "free identifier H is not representable"
    ),
    meta!(
        "40e584e98997164e1b77338ae696000029dd1fe8f0cc3ba753d0517b0ef74e9c",
        "generateSpec",
        "z#generateSpec#0",
        "AST node ArrowFunctionExpression is not representable"
    ),
    meta!(
        "4bba28e7b0bec9d3b055fd4b3fe7c26d8ca69ab2d4777aae6e7c7266cf5756d0",
        "generateSpec",
        "dotnet#generateSpec#0",
        "spread calls are unsupported"
    ),
    meta!(
        "5d4536ed0dfc0baf31213c9ad56a6c76fb78350512cf98c1299b997ec0b32df8",
        "generateSpec",
        "fig/1.0.0#generateSpec#9",
        "fig 1.4.1 run generateSpec closes over graphql helper x"
    ),
    meta!(
        "65622e2681477974a4e8314edfdc88e3550d5201e2948f1a9f3e5b99326208b4",
        "generateSpec",
        "fig/2.0.0#generateSpec#1",
        "free identifier G is not representable"
    ),
    meta!(
        "682679dc8ba7e238f7470ed62e6215129b2901203bbd3c36385dab9f0872463e",
        "generateSpec",
        "serverless#generateSpec#0",
        "free identifier ps is not representable"
    ),
    meta!(
        "7569b7431758d1decc7ac769e222a32a280439661b93d963bcc4e16134e95d62",
        "generateSpec",
        "fig/2.0.0#generateSpec#17",
        "fig 2.10.0 cli generateSpec closes over graphql helper P"
    ),
    meta!(
        "7685c9fbc459a05d9f68bf80ede922a9b8e7c4b6968ec9db4dab14407af3e26c",
        "generateSpec",
        "php#generateSpec#0",
        "executeCommand is only valid as a callee"
    ),
    meta!(
        "77bd90323f3a9b1c7f94ea21f7660327748a3fbf5f6c988672501f326d5b0d59",
        "generateSpec",
        "composer#generateSpec#0",
        "helper y references host object require"
    ),
    meta!(
        "960fa21e9aabac50aa83fe21530721cf8ac8149d4c1fcde03a70768a55fd2b36",
        "generateSpec",
        "task#generateSpec#0",
        "free identifier b is not representable"
    ),
    meta!(
        "ac3c6daaedebddcfa9871f54fa397f2d61807997debd7ea8b051ac516b94c89d",
        "generateSpec",
        "fig/1.0.0#generateSpec#1",
        "free identifier N is not representable"
    ),
    meta!(
        "b690636bab463d831ea84c645c151e63a938765f3e3b16f08bf4254d3721900a",
        "generateSpec",
        "magento#generateSpec#0",
        "executeCommand is only valid as a callee"
    ),
    meta!(
        "c89e4d5b3f8b94c4ba519c27308fb2e0855928eb15f15cb254945cc084147831",
        "generateSpec",
        "cargo#generateSpec#0",
        "hook parameters must be simple identifiers"
    ),
    meta!(
        "d56f9ed1b9eb112da968b87d8ecbd2a2c24d31c4b4ee6a1512aaf1f103546dc3",
        "generateSpec",
        "pnpm#generateSpec#19",
        "free identifier h is not representable"
    ),
    meta!(
        "db71cdeee0cb1c325e95fcee6db9e70169915a026edb6bc5da8e5b0073d44f38",
        "generateSpec",
        "pnpm#generateSpec#0",
        "AST node FunctionExpression is not representable"
    ),
    meta!(
        "de30ca8db08f693bea3d309d4ac7d974534296b3cba283840927ad716a56295e",
        "generateSpec",
        "nx#generateSpec#0",
        "executeCommand is only valid as a callee"
    ),
    meta!(
        "f4eecad7e6457e73322dbcf599e0c63cb1ef8ec83bb1fc7b0982642b81007465",
        "generateSpec",
        "fig/2.0.0#generateSpec#16",
        "helper parameters must be simple identifiers"
    ),
];

#[derive(Serialize)]
struct AdapterCatalogFile {
    version: u64,
    kind: &'static str,
    adapters: Vec<AdapterCatalogRow>,
}

#[derive(Serialize)]
struct AdapterCatalogRow {
    #[serde(rename = "bodySha256")]
    body_sha256: &'static str,
    field: &'static str,
    #[serde(rename = "representativeHookId")]
    representative_hook_id: &'static str,
    reason: &'static str,
}

/// JSON catalog written by `dump-adapters` and read by classify/compile.
pub fn dump_native_adapter_catalog() -> String {
    let mut adapters: Vec<AdapterCatalogRow> = ADAPTERS
        .iter()
        .map(|entry| AdapterCatalogRow {
            body_sha256: entry.body_sha256,
            field: entry.field,
            representative_hook_id: entry.representative_hook_id,
            reason: entry.reason,
        })
        .collect();
    adapters.sort_by(|left, right| {
        left.field
            .cmp(right.field)
            .then_with(|| left.body_sha256.cmp(right.body_sha256))
    });
    let catalog = AdapterCatalogFile {
        version: ADAPTERS_VERSION,
        kind: ADAPTERS_KIND,
        adapters,
    };
    format!("{}\n", serde_json::to_string_pretty(&catalog).expect("catalog"))
}

pub fn native_adapter_catalog_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/native-hooks/adapters.json")
}

mod bunx_names;
mod custom;
mod effect;
mod eval;
mod filter;
mod generate_spec;
mod git_config_keys;
mod post_process;
mod turbo_icon;

pub(crate) use effect::{AdapterExecRequest, AdapterExecResult, HookSite};
pub(crate) use eval::{AdapterError, throw as adapter_throw};

pub(crate) fn evaluate_post_process(
    body_sha256: &str,
    stdout: &str,
    site: HookSite<'_>,
) -> Option<Result<serde_json::Value, String>> {
    let function = post_process_fn(body_sha256)?;
    Some(adapter_json_result(function(stdout, site)))
}

pub(crate) fn evaluate_filter(
    body_sha256: &str,
    suggestions: &[serde_json::Value],
    site: HookSite<'_>,
) -> Option<Result<serde_json::Value, String>> {
    let function = filter_fn(body_sha256)?;
    Some(adapter_json_result(function(suggestions, site)))
}

fn filter_fn(body_sha256: &str) -> Option<fn(&[serde_json::Value], HookSite<'_>) -> eval::AdapterResult> {
    Some(match body_sha256 {
        "16eb363f9957097622cbe2ed626cfc4859c24b797e5e2acff58cf40f4f9fbbad" => filter::direnv_envrc,
        _ => return None,
    })
}

pub(crate) fn evaluate_custom(
    body_sha256: &str,
    tokens: &[String],
    exec: &effect::AdapterExec<'_>,
    context: &crate::hook_types::HookContext,
) -> Option<Result<serde_json::Value, String>> {
    Some(adapter_json_result(custom::evaluate(
        body_sha256,
        tokens,
        exec,
        context,
    )?))
}

pub(crate) fn evaluate_generate_spec(
    body_sha256: &str,
    tokens: &[String],
    exec: &effect::AdapterExec<'_>,
    context: &crate::hook_types::HookContext,
) -> Option<Result<serde_json::Value, String>> {
    Some(adapter_json_result(generate_spec::evaluate(
        body_sha256,
        tokens,
        exec,
        context,
    )?))
}

fn adapter_json_result(result: eval::AdapterResult) -> Result<serde_json::Value, String> {
    result.map_err(|error| error.js_class.unwrap_or("Error").to_string())
}

fn post_process_fn(body_sha256: &str) -> Option<fn(&str, HookSite<'_>) -> eval::AdapterResult> {
    Some(match body_sha256 {
        "06cf60a4db4009e789aa2efbfd2e826a8fc7be284a20bc56a257ff0ba2524a33" => {
            |stdout, _| post_process::bunx_npx(stdout)
        },
        "08c4a4a0e91a87111822336c4aba36b82aa89bef9faac02831e7c1bafb0eeb1c" => {
            |stdout, _| post_process::just_assignments(stdout)
        },
        "1db94727eae43e429172bdb647fcdce51562bc830797579fdd211681742b949b" => {
            |stdout, _| post_process::rush_projects(stdout)
        },
        "20afa25251f32cd950d69142214fb6b725cca32e75676c486610ddabb6200d58" => {
            |stdout, _| post_process::dcli_devices(stdout)
        },
        "40446df6c189303738556d8b980c04a816d639d9ed1576c2f1eee6df70b1ff3d" => post_process::limactl_instances,
        "40f323a2aaed65f89013fd38fc15cd73181b00b577418036d25981f47b98ebb8" => {
            |stdout, _| post_process::git_remotes(stdout)
        },
        "46cfb8ef98d11e187a3fb9a8756cf911956bd5f5bc48d22ce938faee7860f35b" => {
            |stdout, _| post_process::react_native_devices(stdout)
        },
        "47dd9ebffc5ba15a140bafd0c09167f71b38de57bb8782036e4edd0ba8049e89" => post_process::asdf_plugins,
        "566065d403b4e22dba74ec84e8e64ef2c541ce984fef4051e41baaf5a8941120" => {
            |stdout, _| post_process::cargo_deps(stdout)
        },
        "57813e77ada7aad9de83a53034b622f02d180c989cc9b6688aec5f271b614906" => {
            |stdout, _| post_process::fnm_list(stdout)
        },
        "581af18c6c4ccb098615187809a15e6892771178da47cb9c7016272dc18fb572" => {
            |stdout, _| post_process::dcli_access_keys(stdout)
        },
        "599dd22a2fdc7873a899448dd4c11ffce9c0742c9a64e930819f57990b0c126f" => {
            |stdout, _| post_process::rustup_channels(stdout)
        },
        "5b988a428bdc5060c8f035d9d480ea81be6e411de28351871118d9e8bc7d4978" => {
            |stdout, _| post_process::tldr_pages(stdout)
        },
        "5cde47b72c7c8040bea8146f4c5bd7267bb7f07bb53b84964901d097f94b2d37" => post_process::rustup_toolchains,
        "61d0b086797b88cb187f1bcc5b3a82c66b081a9756fde0e10cfde9fb90f5eabf" => post_process::rustup_targets,
        "66349787d8ae1c1f6b8b895a43758316cff3ef10d8f8c70a9eaace0b079c937c" => {
            |stdout, _| post_process::precommit_hooks(stdout)
        },
        "7684a7c68524c7c00f9309417061576eb590152382aea725b4687ea6824cfe6c" => post_process::brew_services,
        "775c0c96e9204671a2a03e3ec4839bfd73f1e9fdec79ce11f7d2b1abf9120c67" => {
            |stdout, _| post_process::deno_url(stdout)
        },
        "83c37762b8e6a3fb3dfef347ac4d34f35174c2ae0483908a2101a1c521a2ef1f" => {
            |stdout, site| post_process::yarn_deps(stdout, site.tokens)
        },
        "86847ea037183bc60ece6f12b5dc2764beb807a76a4930c2a28412a63d96e174" => {
            |stdout, _| post_process::yo_generators(stdout)
        },
        "86e2f49405e79a81d3c4fd402009b27d5e352d1e80d69ee496dda22abc8c65b2" => {
            |stdout, _| post_process::taskwarrior_a(stdout)
        },
        "97aa92d325bb5a2273a65c8204a97bbe4be8598b9d41cf129d8fa554d510fbf3" => post_process::asdf_versions,
        "9e08e5a9bac41e7def677da3f7fc812fc90377d437d8121a05c1f661fecfb32a" => {
            |stdout, _| post_process::open_apps(stdout)
        },
        "a148afe726cd7409a53bcfe722708bc9e35a5e9f7a409817a22783fe77527df3" => {
            |stdout, _| post_process::snaplet_cloud(stdout)
        },
        "a2343033ad7ec234dc9cd89f566c5c9024a725655738aa583562d0c4557f0de5" => {
            |stdout, _| post_process::sake_groups(stdout)
        },
        "a8bce6f4bcf79ed952f5325ec0b5c7f1041eb0e10320fbd9beb629efb4071bbe" => {
            |stdout, _| post_process::taskwarrior_b(stdout)
        },
        "b0575d96c049ffec2de9ce491f5319e3ad61c3c908d0dec5554aeda00ed971c7" => post_process::tailscale_peers,
        "b3739a280026f1e961efaf2ded7bf918e22c909af433ec86f2eec02c6aadeca1" => {
            |stdout, _| post_process::snaplet_backups(stdout)
        },
        "b3b72135b863b89e29cf44274d5e381daf51d9c53c3313cc68096fc1f2af8177" => {
            |stdout, site| post_process::pnpm_deps(stdout, site.tokens)
        },
        "baa2ab8ac2cd27aaf62bd6b24ddd1734a395dcbab0dfed6ad6ddf108d19ac709" => {
            |stdout, _| post_process::tccutil_services(stdout)
        },
        "bcb6a45f08256f9fd34e48ecbfb13cd9b5d825e96e308131538d16cb2f5400bc" => {
            |stdout, _| post_process::turbo_pipeline(stdout)
        },
        "c3fac3c2f48178fe13af582ab93d7b9bfc579951c78a805946b6563c415e8848" => {
            |stdout, _| post_process::just_recipe_list(stdout)
        },
        "ca1383d43cf172af5d0f743e6933ef8e7154147c5ff549ad1b6fc4a6e88e90be" => {
            |stdout, _| post_process::taskwarrior_c(stdout)
        },
        "d060a61ead077da1a1b2a779f8670f87f55010a22a34bdd3f8b8545378d0833e" => {
            |stdout, _| post_process::git_remotes(stdout)
        },
        "d3e93ba8a10aa6f82f74dd3df6cc51e81d815eb8e36072ed084d9a2dddcd6f61" => {
            |stdout, site| post_process::just_recipes_arity(stdout, site.tokens)
        },
        "de5329d88e6a3667531eace7c10a57034bb8ec9e643f910095a0baa80235de20" => {
            |stdout, _| post_process::git_config(stdout)
        },
        "deba5217b1bd370de0396ae66042361a21ebd372bc0e308858dedca94ec79986" => {
            |stdout, _| post_process::vr_scripts(stdout)
        },
        "e0ff02cf4190764a1e6043c0ace62aaf37a63bffd693744f1072c87df04c710d" => {
            |stdout, _| post_process::kubectl_cronjob(stdout)
        },
        "e66f8b0736bf23a063bda8970e1de8b9ac8f72328a53ac15671982af49b7f468" => {
            |stdout, _| post_process::snaplet_status(stdout)
        },
        "e8289375fc0901b1f363b7525760614e99748125598c35e172710d7446292842" => {
            |stdout, site| post_process::deno_docs(stdout, site.tokens)
        },
        "e8a02695049c0e386fb7345f0e54d5bd2d2ce1c04481e228e5e9f2bf4f583c14" => {
            |stdout, _| post_process::gource_displays(stdout)
        },
        "f04211ce9cc3b53755c0429dfc0fcaac7b07fe7d4a7d9021fc2c3c11cf5f6adb" => post_process::cf_lines,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::Value as JsonValue;

    use super::*;
    use crate::hook_baseline::{BaselineCase, Expected, load_all};
    use crate::native_adapters::effect::{expected_from_field, mock_exec_from_rules};
    use crate::native_adapters::eval::{AdapterResult, expected_from_adapter};

    #[derive(Clone, Copy)]
    enum AdapterFn {
        PostProcess(fn(&str, HookSite<'_>) -> AdapterResult),
        Filter(fn(&[JsonValue], HookSite<'_>) -> AdapterResult),
        Custom,
        GenerateSpec,
    }

    fn adapter_eval(field: &str, body_sha256: &str) -> Option<AdapterFn> {
        use crate::native_adapters::{filter, post_process};
        Some(match (field, body_sha256) {
            ("filterTemplateSuggestions", "16eb363f9957097622cbe2ed626cfc4859c24b797e5e2acff58cf40f4f9fbbad") => {
                AdapterFn::Filter(filter::direnv_envrc)
            },
            ("postProcess", "06cf60a4db4009e789aa2efbfd2e826a8fc7be284a20bc56a257ff0ba2524a33") => {
                AdapterFn::PostProcess(|stdout, _| post_process::bunx_npx(stdout))
            },
            ("postProcess", "08c4a4a0e91a87111822336c4aba36b82aa89bef9faac02831e7c1bafb0eeb1c") => {
                AdapterFn::PostProcess(|stdout, _| post_process::just_assignments(stdout))
            },
            ("postProcess", "1db94727eae43e429172bdb647fcdce51562bc830797579fdd211681742b949b") => {
                AdapterFn::PostProcess(|stdout, _| post_process::rush_projects(stdout))
            },
            ("postProcess", "20afa25251f32cd950d69142214fb6b725cca32e75676c486610ddabb6200d58") => {
                AdapterFn::PostProcess(|stdout, _| post_process::dcli_devices(stdout))
            },
            ("postProcess", "40446df6c189303738556d8b980c04a816d639d9ed1576c2f1eee6df70b1ff3d") => {
                AdapterFn::PostProcess(post_process::limactl_instances)
            },
            ("postProcess", "40f323a2aaed65f89013fd38fc15cd73181b00b577418036d25981f47b98ebb8") => {
                AdapterFn::PostProcess(|stdout, _| post_process::git_remotes(stdout))
            },
            ("postProcess", "46cfb8ef98d11e187a3fb9a8756cf911956bd5f5bc48d22ce938faee7860f35b") => {
                AdapterFn::PostProcess(|stdout, _| post_process::react_native_devices(stdout))
            },
            ("postProcess", "47dd9ebffc5ba15a140bafd0c09167f71b38de57bb8782036e4edd0ba8049e89") => {
                AdapterFn::PostProcess(post_process::asdf_plugins)
            },
            ("postProcess", "566065d403b4e22dba74ec84e8e64ef2c541ce984fef4051e41baaf5a8941120") => {
                AdapterFn::PostProcess(|stdout, _| post_process::cargo_deps(stdout))
            },
            ("postProcess", "57813e77ada7aad9de83a53034b622f02d180c989cc9b6688aec5f271b614906") => {
                AdapterFn::PostProcess(|stdout, _| post_process::fnm_list(stdout))
            },
            ("postProcess", "581af18c6c4ccb098615187809a15e6892771178da47cb9c7016272dc18fb572") => {
                AdapterFn::PostProcess(|stdout, _| post_process::dcli_access_keys(stdout))
            },
            ("postProcess", "599dd22a2fdc7873a899448dd4c11ffce9c0742c9a64e930819f57990b0c126f") => {
                AdapterFn::PostProcess(|stdout, _| post_process::rustup_channels(stdout))
            },
            ("postProcess", "5b988a428bdc5060c8f035d9d480ea81be6e411de28351871118d9e8bc7d4978") => {
                AdapterFn::PostProcess(|stdout, _| post_process::tldr_pages(stdout))
            },
            ("postProcess", "5cde47b72c7c8040bea8146f4c5bd7267bb7f07bb53b84964901d097f94b2d37") => {
                AdapterFn::PostProcess(post_process::rustup_toolchains)
            },
            ("postProcess", "61d0b086797b88cb187f1bcc5b3a82c66b081a9756fde0e10cfde9fb90f5eabf") => {
                AdapterFn::PostProcess(post_process::rustup_targets)
            },
            ("postProcess", "66349787d8ae1c1f6b8b895a43758316cff3ef10d8f8c70a9eaace0b079c937c") => {
                AdapterFn::PostProcess(|stdout, _| post_process::precommit_hooks(stdout))
            },
            ("postProcess", "7684a7c68524c7c00f9309417061576eb590152382aea725b4687ea6824cfe6c") => {
                AdapterFn::PostProcess(post_process::brew_services)
            },
            ("postProcess", "775c0c96e9204671a2a03e3ec4839bfd73f1e9fdec79ce11f7d2b1abf9120c67") => {
                AdapterFn::PostProcess(|stdout, _| post_process::deno_url(stdout))
            },
            ("postProcess", "83c37762b8e6a3fb3dfef347ac4d34f35174c2ae0483908a2101a1c521a2ef1f") => {
                AdapterFn::PostProcess(|stdout, site| post_process::yarn_deps(stdout, site.tokens))
            },
            ("postProcess", "86847ea037183bc60ece6f12b5dc2764beb807a76a4930c2a28412a63d96e174") => {
                AdapterFn::PostProcess(|stdout, _| post_process::yo_generators(stdout))
            },
            ("postProcess", "86e2f49405e79a81d3c4fd402009b27d5e352d1e80d69ee496dda22abc8c65b2") => {
                AdapterFn::PostProcess(|stdout, _| post_process::taskwarrior_a(stdout))
            },
            ("postProcess", "97aa92d325bb5a2273a65c8204a97bbe4be8598b9d41cf129d8fa554d510fbf3") => {
                AdapterFn::PostProcess(post_process::asdf_versions)
            },
            ("postProcess", "9e08e5a9bac41e7def677da3f7fc812fc90377d437d8121a05c1f661fecfb32a") => {
                AdapterFn::PostProcess(|stdout, _| post_process::open_apps(stdout))
            },
            ("postProcess", "a148afe726cd7409a53bcfe722708bc9e35a5e9f7a409817a22783fe77527df3") => {
                AdapterFn::PostProcess(|stdout, _| post_process::snaplet_cloud(stdout))
            },
            ("postProcess", "a2343033ad7ec234dc9cd89f566c5c9024a725655738aa583562d0c4557f0de5") => {
                AdapterFn::PostProcess(|stdout, _| post_process::sake_groups(stdout))
            },
            ("postProcess", "a8bce6f4bcf79ed952f5325ec0b5c7f1041eb0e10320fbd9beb629efb4071bbe") => {
                AdapterFn::PostProcess(|stdout, _| post_process::taskwarrior_b(stdout))
            },
            ("postProcess", "b0575d96c049ffec2de9ce491f5319e3ad61c3c908d0dec5554aeda00ed971c7") => {
                AdapterFn::PostProcess(post_process::tailscale_peers)
            },
            ("postProcess", "b3739a280026f1e961efaf2ded7bf918e22c909af433ec86f2eec02c6aadeca1") => {
                AdapterFn::PostProcess(|stdout, _| post_process::snaplet_backups(stdout))
            },
            ("postProcess", "b3b72135b863b89e29cf44274d5e381daf51d9c53c3313cc68096fc1f2af8177") => {
                AdapterFn::PostProcess(|stdout, site| post_process::pnpm_deps(stdout, site.tokens))
            },
            ("postProcess", "baa2ab8ac2cd27aaf62bd6b24ddd1734a395dcbab0dfed6ad6ddf108d19ac709") => {
                AdapterFn::PostProcess(|stdout, _| post_process::tccutil_services(stdout))
            },
            ("postProcess", "bcb6a45f08256f9fd34e48ecbfb13cd9b5d825e96e308131538d16cb2f5400bc") => {
                AdapterFn::PostProcess(|stdout, _| post_process::turbo_pipeline(stdout))
            },
            ("postProcess", "c3fac3c2f48178fe13af582ab93d7b9bfc579951c78a805946b6563c415e8848") => {
                AdapterFn::PostProcess(|stdout, _| post_process::just_recipe_list(stdout))
            },
            ("postProcess", "ca1383d43cf172af5d0f743e6933ef8e7154147c5ff549ad1b6fc4a6e88e90be") => {
                AdapterFn::PostProcess(|stdout, _| post_process::taskwarrior_c(stdout))
            },
            ("postProcess", "d060a61ead077da1a1b2a779f8670f87f55010a22a34bdd3f8b8545378d0833e") => {
                AdapterFn::PostProcess(|stdout, _| post_process::git_remotes(stdout))
            },
            ("postProcess", "d3e93ba8a10aa6f82f74dd3df6cc51e81d815eb8e36072ed084d9a2dddcd6f61") => {
                AdapterFn::PostProcess(|stdout, site| post_process::just_recipes_arity(stdout, site.tokens))
            },
            ("postProcess", "de5329d88e6a3667531eace7c10a57034bb8ec9e643f910095a0baa80235de20") => {
                AdapterFn::PostProcess(|stdout, _| post_process::git_config(stdout))
            },
            ("postProcess", "deba5217b1bd370de0396ae66042361a21ebd372bc0e308858dedca94ec79986") => {
                AdapterFn::PostProcess(|stdout, _| post_process::vr_scripts(stdout))
            },
            ("postProcess", "e0ff02cf4190764a1e6043c0ace62aaf37a63bffd693744f1072c87df04c710d") => {
                AdapterFn::PostProcess(|stdout, _| post_process::kubectl_cronjob(stdout))
            },
            ("postProcess", "e66f8b0736bf23a063bda8970e1de8b9ac8f72328a53ac15671982af49b7f468") => {
                AdapterFn::PostProcess(|stdout, _| post_process::snaplet_status(stdout))
            },
            ("postProcess", "e8289375fc0901b1f363b7525760614e99748125598c35e172710d7446292842") => {
                AdapterFn::PostProcess(|stdout, site| post_process::deno_docs(stdout, site.tokens))
            },
            ("postProcess", "e8a02695049c0e386fb7345f0e54d5bd2d2ce1c04481e228e5e9f2bf4f583c14") => {
                AdapterFn::PostProcess(|stdout, _| post_process::gource_displays(stdout))
            },
            ("postProcess", "f04211ce9cc3b53755c0429dfc0fcaac7b07fe7d4a7d9021fc2c3c11cf5f6adb") => {
                AdapterFn::PostProcess(post_process::cf_lines)
            },
            (field, sha)
                if ADAPTERS
                    .iter()
                    .any(|entry| entry.field == field && entry.body_sha256 == sha) =>
            {
                match field {
                    "custom" => AdapterFn::Custom,
                    "generateSpec" => AdapterFn::GenerateSpec,
                    _ => return None,
                }
            },
            _ => return None,
        })
    }

    fn evaluate_adapter(field: &str, body_sha256: &str, case: &BaselineCase) -> Option<Expected> {
        let eval = adapter_eval(field, body_sha256)?;
        let exec = mock_exec_from_rules(&case.exec);
        let result = match eval {
            AdapterFn::PostProcess(function) => {
                let stdout = case.args.first().and_then(JsonValue::as_str).unwrap_or("");
                let tokens = case
                    .args
                    .get(1)
                    .and_then(JsonValue::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .map(|item| item.as_str().unwrap_or("").to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                // The T1.2 cases carry no generator script, so a
                // script-keyed adapter sees its representative site.
                function(stdout, HookSite::tokens(&tokens))
            },
            AdapterFn::Filter(function) => {
                let suggestions = case
                    .args
                    .first()
                    .and_then(JsonValue::as_array)
                    .cloned()
                    .unwrap_or_default();
                function(&suggestions, HookSite::default())
            },
            AdapterFn::Custom => {
                let tokens = case
                    .args
                    .first()
                    .and_then(JsonValue::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .map(|item| item.as_str().unwrap_or("").to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                crate::native_adapters::custom::evaluate(body_sha256, &tokens, &exec, &case.context)?
            },
            AdapterFn::GenerateSpec => {
                let tokens = case
                    .args
                    .first()
                    .and_then(JsonValue::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .map(|item| item.as_str().unwrap_or("").to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                crate::native_adapters::generate_spec::evaluate(body_sha256, &tokens, &exec, &case.context)?
            },
        };
        Some(match eval {
            AdapterFn::Custom | AdapterFn::GenerateSpec => expected_from_field(field, result),
            _ => expected_from_adapter(result),
        })
    }

    #[test]
    fn adapters_json_matches_registry() {
        let path = native_adapter_catalog_path();
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            committed,
            dump_native_adapter_catalog(),
            "adapters.json is stale; run `cargo run -p ec_engine --example dump-adapters`"
        );
    }

    #[test]
    fn native_adapters_baseline_parity() {
        let baselines = load_all().expect("T1.2 baselines");
        let mut compared = 0usize;
        let mut failures = Vec::new();
        let mut seen = 0usize;
        for baseline in &baselines {
            if adapter_eval(&baseline.field, &baseline.body_sha256).is_none() {
                continue;
            }
            seen += 1;
            for case in &baseline.cases {
                if matches!(case.expected, Expected::Timeout { .. }) {
                    continue;
                }
                let Some(actual) = evaluate_adapter(&baseline.field, &baseline.body_sha256, case) else {
                    failures.push(format!("{}:{} missing adapter", baseline.field, baseline.body_sha256));
                    continue;
                };
                if actual == case.expected {
                    compared += 1;
                } else {
                    failures.push(format!(
                        "{} {} {} actual={actual:?} expected={:?}",
                        baseline.field, baseline.body_sha256, case.id, case.expected
                    ));
                }
            }
        }
        assert_eq!(
            seen,
            ADAPTERS.len(),
            "every registered adapter must have a T1.2 baseline"
        );
        assert!(
            failures.is_empty(),
            "native adapter baseline mismatches ({}): {}",
            failures.len(),
            failures.iter().take(20).cloned().collect::<Vec<_>>().join(" | ")
        );
        assert!(
            compared >= ADAPTERS.len(),
            "native adapter baseline parity compared {compared} cases"
        );
    }
}
