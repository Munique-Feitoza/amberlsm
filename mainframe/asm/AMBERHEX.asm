***********************************************************************
* AMBERHEX - ponto de extensão para benchmark HLASM.                  *
*                                                                     *
* A rotina recebe um endereço de buffer em R1 e o comprimento em R0.  *
* A primeira versão apenas valida argumentos e retorna RC=0.           *
* O benchmark deve comparar esta rotina com a implementação Rust antes *
* de substituir qualquer caminho de produção.                          *
***********************************************************************
AMBERHEX CSECT
         AMODE 31
         RMODE ANY
         USING *,R15
         LR    R15,R1
         LTR   R15,R15
         BNZ   VALID
         LA    R15,4
         BR    R14
VALID    XR    R15,R15
         BR    R14
         END   AMBERHEX
