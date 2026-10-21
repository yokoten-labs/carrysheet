section Section1;

shared Sales = let
    Source = Sql.Database("sql01.corp.example", "Sales"),
    Orders = Source{[Schema="dbo", Item="Orders"]}[Data]
in
    Orders;

// Rates come from a signed URL; the signature is fake.
shared #"Daily Rates" = let
    Source = Json.Document(Web.Contents("https://rates.example.com/v1/daily?region=eu&sig=m-sig-secret", [Headers=[Authorization="Bearer m-bearer-secret", #"x-api-key"="m-api-key-secret"]]))
in
    Source;

shared Warehouse = Odbc.Query("dsn=Warehouse;uid=reporter;pwd=m-odbc-secret;", "select * from stock");

[ Description = "Old; kept for reference" ] shared Legacy = OleDb.DataSource("Provider=SQLOLEDB;Data Source=old01;Password=""m-quoted;secret"";", [Query="select 1"]);
