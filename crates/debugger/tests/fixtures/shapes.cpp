#include <map>
#include <memory>
#include <string>
#include <vector>

struct Node { int key; Node* left; Node* right; };
struct ListNode { int value; ListNode* next; };
struct DNode { int value; DNode* prev; DNode* next; };
struct Point { int x, y; };
struct TNode { std::string name; std::vector<std::unique_ptr<TNode>> kids; };

int calls = 7;
Point origin{3, 4};

Node* insert(Node* n, int k) {
    if (!n) return new Node{k, nullptr, nullptr};
    if (k < n->key) n->left = insert(n->left, k);
    else n->right = insert(n->right, k);
    return n;
}

int main() {
    Node* root = nullptr;
    int keys[] = {50, 30, 70, 20, 40, 60, 80, 35};
    for (int k : keys) root = insert(root, k);

    ListNode* head = nullptr;
    for (int i = 5; i > 0; --i) head = new ListNode{i * 10, head};

    DNode* d1 = new DNode{1, nullptr, nullptr};
    DNode* d2 = new DNode{2, d1, nullptr};
    DNode* d3 = new DNode{3, d2, nullptr};
    d1->next = d2; d2->next = d3;

    std::vector<int> v{4, 8, 15, 16, 23, 42};
    std::vector<Node*> picks{root->left, root->right, nullptr};
    int grid[3][4] = {{1, 2, 3, 4}, {5, 6, 7, 8}, {9, 10, 11, 12}};
    Point pts[2] = {{1, 2}, {3, 4}};
    std::map<std::string, int> ages{{"ada", 36}, {"alan", 41}};
    auto tree = std::make_unique<TNode>();
    tree->name = "root";
    tree->kids.push_back(std::make_unique<TNode>(TNode{"a", {}}));
    tree->kids.push_back(std::make_unique<TNode>(TNode{"b", {}}));
    ListNode* cyc = new ListNode{1, nullptr};
    cyc->next = new ListNode{2, cyc};
    const char* greeting = "hello";
    int total = 0;
    for (int x : v) total += x;
    return total + grid[1][1] + pts[0].x + ages["ada"] + (greeting[0] == 'h') + (cyc != nullptr) + calls + origin.y;
}
