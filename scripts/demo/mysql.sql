-- Rowglow demo database (MySQL / MariaDB)
SET SESSION cte_max_recursion_depth = 100000;
CREATE TABLE customers (
  id          BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
  email       VARCHAR(190) NOT NULL UNIQUE,
  full_name   VARCHAR(120) NOT NULL,
  country     CHAR(2) NOT NULL DEFAULT 'US',
  marketing   TINYINT(1) NOT NULL DEFAULT 0,
  metadata    JSON NULL,
  created_at  DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
) COMMENT='People who bought something';

CREATE TABLE products (
  id        INT AUTO_INCREMENT PRIMARY KEY,
  sku       VARCHAR(20) NOT NULL UNIQUE,
  name      VARCHAR(120) NOT NULL,
  price     DECIMAL(10,2) NOT NULL,
  in_stock  INT NOT NULL DEFAULT 0,
  CONSTRAINT price_positive CHECK (price >= 0)
);

CREATE TABLE orders (
  id           BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
  customer_id  BIGINT UNSIGNED NOT NULL,
  status       ENUM('pending','paid','shipped','refunded') NOT NULL DEFAULT 'pending',
  total        DECIMAL(12,2) NOT NULL DEFAULT 0,
  note         TEXT NULL,
  placed_at    DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  KEY orders_status_placed (status, placed_at),
  CONSTRAINT orders_customer_fk FOREIGN KEY (customer_id) REFERENCES customers(id) ON DELETE CASCADE
);

CREATE TABLE order_items (
  order_id    BIGINT UNSIGNED NOT NULL,
  product_id  INT NOT NULL,
  quantity    INT NOT NULL,
  unit_price  DECIMAL(10,2) NOT NULL,
  PRIMARY KEY (order_id, product_id),
  CONSTRAINT items_order_fk FOREIGN KEY (order_id) REFERENCES orders(id) ON DELETE CASCADE,
  CONSTRAINT items_product_fk FOREIGN KEY (product_id) REFERENCES products(id)
);

INSERT INTO customers (email, full_name, country, marketing, metadata, created_at)
WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 2000)
SELECT CONCAT('user', n, '@example.com'),
       CONCAT(ELT(1 + (n % 6), 'Ada Lovelace','Grace Hopper','Alan Turing','Barbara Liskov','Ken Thompson','Margaret Hamilton'), ' ', n),
       ELT(1 + (n % 5), 'US','DE','AL','JP','XK'),
       n % 3 = 0,
       IF(n % 4 = 0, JSON_OBJECT('plan', 'trial', 'score', n % 100), NULL),
       NOW() - INTERVAL n HOUR
FROM seq;

INSERT INTO products (sku, name, price, in_stock)
WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 60)
SELECT CONCAT('SKU-', LPAD(n, 4, '0')), CONCAT(ELT(1 + (n % 4), 'Lantern','Compass','Notebook','Quill'), ' ', n), ROUND(5 + RAND() * 195, 2), FLOOR(RAND() * 500)
FROM seq;

INSERT INTO orders (customer_id, status, total, note, placed_at)
WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 6000)
SELECT 1 + (n % 2000), ELT(1 + (n % 4), 'pending','paid','shipped','refunded'), ROUND(RAND() * 400, 2), IF(n % 17 = 0, 'Gift wrap please', NULL), NOW(3) - INTERVAL n MINUTE
FROM seq;

INSERT IGNORE INTO order_items (order_id, product_id, quantity, unit_price)
SELECT o.id, 1 + (o.id % 60), 1 + (o.id % 3), p.price FROM orders o JOIN products p ON p.id = 1 + (o.id % 60);

CREATE VIEW customer_revenue AS
SELECT c.id, c.full_name, c.country, COUNT(o.id) AS orders, COALESCE(SUM(o.total), 0) AS revenue
FROM customers c LEFT JOIN orders o ON o.customer_id = c.id AND o.status IN ('paid','shipped')
GROUP BY c.id, c.full_name, c.country;
