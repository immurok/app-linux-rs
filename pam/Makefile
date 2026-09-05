CC ?= gcc
CFLAGS = -fPIC -Wall -Wextra -O2
TARGET = pam_immurok.so

all: $(TARGET)

$(TARGET): pam_immurok.c fp_policy.h socket_trust.h
	$(CC) $(CFLAGS) -shared -o $@ $< -lpam

test: test_fp_policy test_socket_trust
	./test_fp_policy
	./test_socket_trust

test_fp_policy: test_fp_policy.c fp_policy.h
	$(CC) -Wall -Wextra -O2 -o $@ test_fp_policy.c

test_socket_trust: test_socket_trust.c socket_trust.h
	$(CC) -Wall -Wextra -O2 -o $@ test_socket_trust.c

clean:
	rm -f $(TARGET) test_fp_policy test_socket_trust

.PHONY: all test clean
