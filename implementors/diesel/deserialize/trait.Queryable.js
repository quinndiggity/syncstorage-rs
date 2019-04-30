(function() {var implementors = {};
implementors["diesel"] = [];
implementors["syncstorage"] = [{text:"impl&lt;__DB:&nbsp;<a class=\"trait\" href=\"diesel/backend/trait.Backend.html\" title=\"trait diesel::backend::Backend\">Backend</a>, __ST&gt; <a class=\"trait\" href=\"diesel/deserialize/trait.Queryable.html\" title=\"trait diesel::deserialize::Queryable\">Queryable</a>&lt;__ST, __DB&gt; for <a class=\"struct\" href=\"syncstorage/db/params/struct.Batch.html\" title=\"struct syncstorage::db::params::Batch\">Batch</a> <span class=\"where fmt-newline\">where<br>&nbsp;&nbsp;&nbsp;&nbsp;<a class=\"primitive\" href=\"https://doc.rust-lang.org/nightly/std/primitive.tuple.html\">(</a><a class=\"primitive\" href=\"https://doc.rust-lang.org/nightly/std/primitive.i64.html\">i64</a>, <a class=\"struct\" href=\"https://doc.rust-lang.org/nightly/alloc/string/struct.String.html\" title=\"struct alloc::string::String\">String</a>, <a class=\"primitive\" href=\"https://doc.rust-lang.org/nightly/std/primitive.i64.html\">i64</a><a class=\"primitive\" href=\"https://doc.rust-lang.org/nightly/std/primitive.tuple.html\">)</a>: <a class=\"trait\" href=\"diesel/deserialize/trait.Queryable.html\" title=\"trait diesel::deserialize::Queryable\">Queryable</a>&lt;__ST, __DB&gt;,&nbsp;</span>",synthetic:false,types:["syncstorage::db::params::Batch"]},{text:"impl&lt;__DB:&nbsp;<a class=\"trait\" href=\"diesel/backend/trait.Backend.html\" title=\"trait diesel::backend::Backend\">Backend</a>, __ST&gt; <a class=\"trait\" href=\"diesel/deserialize/trait.Queryable.html\" title=\"trait diesel::deserialize::Queryable\">Queryable</a>&lt;__ST, __DB&gt; for <a class=\"struct\" href=\"syncstorage/db/results/struct.GetBso.html\" title=\"struct syncstorage::db::results::GetBso\">GetBso</a> <span class=\"where fmt-newline\">where<br>&nbsp;&nbsp;&nbsp;&nbsp;<a class=\"primitive\" href=\"https://doc.rust-lang.org/nightly/std/primitive.tuple.html\">(</a><a class=\"struct\" href=\"https://doc.rust-lang.org/nightly/alloc/string/struct.String.html\" title=\"struct alloc::string::String\">String</a>, <a class=\"struct\" href=\"syncstorage/db/util/struct.SyncTimestamp.html\" title=\"struct syncstorage::db::util::SyncTimestamp\">SyncTimestamp</a>, <a class=\"struct\" href=\"https://doc.rust-lang.org/nightly/alloc/string/struct.String.html\" title=\"struct alloc::string::String\">String</a>, <a class=\"enum\" href=\"https://doc.rust-lang.org/nightly/core/option/enum.Option.html\" title=\"enum core::option::Option\">Option</a>&lt;<a class=\"primitive\" href=\"https://doc.rust-lang.org/nightly/std/primitive.i32.html\">i32</a>&gt;, <a class=\"primitive\" href=\"https://doc.rust-lang.org/nightly/std/primitive.i64.html\">i64</a><a class=\"primitive\" href=\"https://doc.rust-lang.org/nightly/std/primitive.tuple.html\">)</a>: <a class=\"trait\" href=\"diesel/deserialize/trait.Queryable.html\" title=\"trait diesel::deserialize::Queryable\">Queryable</a>&lt;__ST, __DB&gt;,&nbsp;</span>",synthetic:false,types:["syncstorage::db::results::GetBso"]},{text:"impl&lt;__ST, __DB&gt; <a class=\"trait\" href=\"diesel/deserialize/trait.Queryable.html\" title=\"trait diesel::deserialize::Queryable\">Queryable</a>&lt;__ST, __DB&gt; for <a class=\"struct\" href=\"syncstorage/db/util/struct.SyncTimestamp.html\" title=\"struct syncstorage::db::util::SyncTimestamp\">SyncTimestamp</a> <span class=\"where fmt-newline\">where<br>&nbsp;&nbsp;&nbsp;&nbsp;__DB: <a class=\"trait\" href=\"diesel/backend/trait.Backend.html\" title=\"trait diesel::backend::Backend\">Backend</a>,<br>&nbsp;&nbsp;&nbsp;&nbsp;Self: <a class=\"trait\" href=\"diesel/deserialize/trait.FromSql.html\" title=\"trait diesel::deserialize::FromSql\">FromSql</a>&lt;__ST, __DB&gt;,&nbsp;</span>",synthetic:false,types:["syncstorage::db::util::SyncTimestamp"]},];

            if (window.register_implementors) {
                window.register_implementors(implementors);
            } else {
                window.pending_implementors = implementors;
            }
        
})()
