use proc_macro::TokenStream;
use quote::quote;
use syn::{ext::IdentExt, parse_macro_input, Data, DeriveInput, Error, Fields, LitStr, Result};

/// Map the named fields of a struct to SQLite columns. The default key is `id`.
#[proc_macro_derive(Table, attributes(table))]
pub fn derive_table(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(input).unwrap_or_else(|e| e.into_compile_error().into())
}

fn expand(input: DeriveInput) -> Result<TokenStream> {
    let mut name = None;
    let mut key = String::from("id");
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("table")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                name = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("key") {
                key = meta.value()?.parse::<LitStr>()?.value();
            } else {
                return Err(meta.error("expected name or key"));
            }
            Ok(())
        })?;
    }
    let ident = &input.ident;
    let name =
        name.ok_or_else(|| Error::new_spanned(ident, "expected #[table(name = \"...\")]"))?;
    if name.is_empty()
        || name.contains('\0')
        || name.eq_ignore_ascii_case("hk_documents")
        || name.to_ascii_lowercase().starts_with("sqlite_")
    {
        return Err(Error::new_spanned(
            ident,
            "invalid or reserved SQLite table name",
        ));
    }
    let Data::Struct(data) = &input.data else {
        return Err(Error::new_spanned(
            ident,
            "Table requires a struct with named fields",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(Error::new_spanned(ident, "Table requires named fields"));
    };
    let ids: Vec<_> = fields
        .named
        .iter()
        .map(|f| f.ident.as_ref().unwrap())
        .collect();
    let types: Vec<_> = fields.named.iter().map(|f| &f.ty).collect();
    let names: Vec<_> = ids.iter().map(|id| id.unraw().to_string()).collect();
    if names.iter().any(|n| n.eq_ignore_ascii_case("_rowid_")) {
        return Err(Error::new_spanned(
            ident,
            "_rowid_ is reserved for insertion order",
        ));
    }
    let index = names
        .iter()
        .position(|n| n == &key)
        .ok_or_else(|| Error::new_spanned(ident, format!("missing primary key field '{key}'")))?;
    let key_type = types[index];
    let key_field = ids[index];
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let store = quote!(::rproxy_kernel::store);
    let sql = quote!(#store::rusqlite);
    Ok(quote! {
        impl #impl_generics #store::Table for #ident #ty_generics #where_clause {
            type Key = <#key_type as #store::PrimaryKey>::Lookup;
            const NAME: &'static str = #name;
            const KEY: &'static str = #key;
            const COLUMNS: &'static [#store::Column] = &[
                #(#store::Column::of::<#types>(#names)),*
            ];
            fn values(&self) -> Vec<&dyn #sql::ToSql> {
                vec![#(&self.#ids),*]
            }
            fn from_row(row: &#sql::Row<'_>) -> #sql::Result<Self> {
                Ok(Self { #(#ids: row.get(#names)?),* })
            }
            fn legacy_id(&self) -> String { self.#key_field.to_string() }
            fn key(&self) -> &Self::Key { &self.#key_field }
        }
    }
    .into())
}
