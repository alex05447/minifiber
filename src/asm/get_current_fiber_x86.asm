.MODEL FLAT, C

PUBLIC get_current_fiber
EXTERN puts:PROC

.code

get_current_fiber PROC PUBLIC
	
	ASSUME  fs:NOTHING
    mov	eax, dword ptr fs:[10h]
	ASSUME  fs:ERROR

	ret

get_current_fiber ENDP
END