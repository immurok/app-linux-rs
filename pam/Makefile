CC ?= gcc
CFLAGS = -fPIC -Wall -Wextra -O2
TARGET = pam_immurok.so

all: $(TARGET)

$(TARGET): pam_immurok.c fp_policy.h
	$(CC) $(CFLAGS) -shared -o $@ $< -lpam

test: test_fp_policy
	./test_fp_policy

test_fp_policy: test_fp_policy.c fp_policy.h
	$(CC) -Wall -Wextra -O2 -o $@ test_fp_policy.c

clean:
	rm -f $(TARGET) test_fp_policy

.PHONY: all test clean
