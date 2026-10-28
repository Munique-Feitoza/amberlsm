      ******************************************************************
      * Fixture COBOL: gera operações do protocolo AmberLSM.           *
      * A saída pode ser redirecionada para amberlsm-protocol.          *
      ******************************************************************
       IDENTIFICATION DIVISION.
       PROGRAM-ID. AMBERFIX.
       ENVIRONMENT DIVISION.
       INPUT-OUTPUT SECTION.
       FILE-CONTROL.
           SELECT INPUT-FILE ASSIGN TO INPUTDD
               ORGANIZATION IS LINE SEQUENTIAL.
       DATA DIVISION.
       FILE SECTION.
       FD INPUT-FILE.
       01 INPUT-RECORD PIC X(200).
       WORKING-STORAGE SECTION.
       01 END-FILE PIC X VALUE 'N'.
       01 OPERATION PIC X(220).
       PROCEDURE DIVISION.
           OPEN INPUT INPUT-FILE
           PERFORM UNTIL END-FILE = 'Y'
               READ INPUT-FILE
                   AT END MOVE 'Y' TO END-FILE
                   NOT AT END
                       MOVE SPACES TO OPERATION
                       STRING 'PUT ' DELIMITED BY SIZE
                              INPUT-RECORD DELIMITED BY SPACE
                              ' 636f626f6c2d6d61696e6672616d65'
                              DELIMITED BY SIZE
                              INTO OPERATION
                       END-STRING
                       DISPLAY OPERATION
               END-READ
           END-PERFORM
           CLOSE INPUT-FILE
           GOBACK.
