PUBLIC get_current_fiber
EXTERN puts:PROC

.code

get_current_fiber PROC PUBLIC
	
    mov rax, qword ptr gs:[20h]

	ret

get_current_fiber ENDP
END