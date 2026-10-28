/* Orquestra uma carga de operações gerada por COBOL. */
parse arg database operations
if database = '' then database = '/z/'userid()'/amberlsm-data'
if operations = '' then operations = '/z/'userid()'/amberlsm/operations.txt'
say 'AmberLSM database: ' database
say 'Operations file: ' operations
/* A execução final depende do caminho do binário disponível no USS. */
address syscall 'chmod 755 ' operations
say 'Fixture preparado; encaminhe operations.txt ao adaptador Rust.'
exit 0
